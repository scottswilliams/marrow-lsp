import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

// These types mirror the marrow-lsp custom request contract EXACTLY. They are
// the wire shapes the server speaks; do not "improve" them or they will drift
// from the server. The TypeScript side never interprets a path — it only ferries
// these segments back to the core, which owns all store semantics.

// Key is a one-field tagged union.
type Key =
  | { int: number }
  | { bool: boolean }
  | { str: string }
  | { date: number }
  | { duration: string }
  | { instant: string }
  | { bytes: string };

// Segment is a root-first path component.
type Segment =
  | { root: string }
  | { key: Key }
  | { field: string }
  | { layer: string }
  | { index: string }
  | { index_key: Key };

type ChildMetadata = {
  role?: string;
  detail?: string;
  type?: string;
  appendSegment?: Segment;
};

type Child =
  | ({ kind: "key"; key: Key } & ChildMetadata)
  | ({ kind: "name"; name: string } & ChildMetadata);

type Presence = "absent" | "value_only" | "children_only" | "value_and_children";

interface SavedRootsResult {
  available: boolean;
  roots: string[];
}

interface SavedChildrenResult {
  available: boolean;
  children: Child[];
  more: boolean;
}

interface SavedGetResult {
  available: boolean;
  presence: Presence;
  value?: string;
  type?: string;
}

const REQUEST_SAVED_ROOTS = "marrow/savedRoots";
const REQUEST_SAVED_CHILDREN = "marrow/savedChildren";
const REQUEST_SAVED_GET = "marrow/savedGet";

// A node in the "Marrow Data" tree. Each real node carries the full root-first
// path to the store location it represents, plus the label/description we show.
// The "unavailable" and "more" nodes are inert placeholders with no path.
type MarrowDataNode = StoreNode | PlaceholderNode;

interface StoreNode {
  readonly kind: "store";
  // Full root-first path identifying this location in the store.
  readonly path: Segment[];
  // Text shown for the tree item (root name, key value, or member name).
  readonly label: string;
  readonly role?: string;
  readonly detail?: string;
  readonly type?: string;
}

interface PlaceholderNode {
  readonly kind: "unavailable" | "more";
  readonly label: string;
}

function isStoreNode(node: MarrowDataNode): node is StoreNode {
  return node.kind === "store";
}

// Render a Key as a short human label for the tree. The store owns real
// semantics; this is display only.
function keyLabel(key: Key): string {
  if ("int" in key) {
    return String(key.int);
  }
  if ("bool" in key) {
    return String(key.bool);
  }
  if ("str" in key) {
    return JSON.stringify(key.str);
  }
  if ("date" in key) {
    return String(key.date);
  }
  if ("duration" in key) {
    return key.duration;
  }
  if ("instant" in key) {
    return key.instant;
  }
  return key.bytes;
}

// Map a server Child plus its parent path to a concrete tree node. The server
// owns saved path semantics, so a child without a canonical append segment is
// rendered as an inert placeholder instead of guessing a path.
function childToNode(parentPath: Segment[], child: Child): MarrowDataNode {
  const appendSegment = child.appendSegment;
  if (appendSegment === undefined) {
    return unavailablePathMetadataNode();
  }
  if (child.kind === "key") {
    return {
      kind: "store",
      path: [...parentPath, appendSegment],
      label: keyLabel(child.key),
      role: child.role,
      detail: child.detail,
      type: child.type,
    };
  }
  return {
    kind: "store",
    path: [...parentPath, appendSegment],
    label: child.name,
    role: child.role,
    detail: child.detail,
    type: child.type,
  };
}

export class MarrowDataProvider implements vscode.TreeDataProvider<MarrowDataNode> {
  private readonly emitter = new vscode.EventEmitter<MarrowDataNode | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  // The shared, running language client, or undefined when the server is down.
  private client: LanguageClient | undefined;

  setClient(client: LanguageClient | undefined): void {
    this.client = client;
    this.refresh();
  }

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(node: MarrowDataNode): vscode.TreeItem {
    if (!isStoreNode(node)) {
      const item = new vscode.TreeItem(node.label, vscode.TreeItemCollapsibleState.None);
      item.contextValue = node.kind;
      if (node.kind === "unavailable") {
        item.iconPath = new vscode.ThemeIcon("circle-slash");
      }
      return item;
    }
    // Store nodes are collapsed by default; expansion lazily fetches children.
    // We deliberately do not pre-fetch presence here — getChildren resolves the
    // value/type description so the inline value is fetched once on expand.
    const item = new vscode.TreeItem(node.label, vscode.TreeItemCollapsibleState.Collapsed);
    item.contextValue = "marrowData";
    const schema = describeSchema(node, true);
    item.description = schema;
    item.tooltip = schema;
    return item;
  }

  async getChildren(node?: MarrowDataNode): Promise<MarrowDataNode[]> {
    const client = this.client;
    if (client === undefined) {
      return [];
    }

    // Top level: list saved roots.
    if (node === undefined) {
      return this.rootNodes(client);
    }

    // Placeholders have no children.
    if (!isStoreNode(node)) {
      return [];
    }

    return this.childNodes(client, node);
  }

  // resolveTreeItem fills in the inline value/type description lazily, only for
  // the items VSCode actually renders. We fetch savedGet here rather than in
  // getChildren so a parent expansion does not issue a savedGet per child up
  // front; the description is resolved when the row becomes visible.
  async resolveTreeItem(
    item: vscode.TreeItem,
    node: MarrowDataNode,
  ): Promise<vscode.TreeItem> {
    const client = this.client;
    if (client === undefined || !isStoreNode(node)) {
      return item;
    }
    try {
      const result = await client.sendRequest<SavedGetResult>(REQUEST_SAVED_GET, {
        path: node.path,
      });
      if (result.available) {
        const value = describeValue(result);
        const schema = describeSchema(node, result.type === undefined);
        const description = joinDescriptions(value, schema);
        item.description = description;
        item.tooltip = description;
      }
    } catch {
      // A failed savedGet leaves the row without an inline value; the tree
      // remains usable. The store is read-only and may be transiently busy.
    }
    return item;
  }

  private async rootNodes(client: LanguageClient): Promise<MarrowDataNode[]> {
    let result: SavedRootsResult;
    try {
      result = await client.sendRequest<SavedRootsResult>(REQUEST_SAVED_ROOTS);
    } catch {
      return [unavailableNode()];
    }
    if (!result.available) {
      return [unavailableNode()];
    }
    return result.roots.map(
      (root): StoreNode => ({
        kind: "store",
        path: [{ root }],
        label: root,
      }),
    );
  }

  private async childNodes(
    client: LanguageClient,
    node: StoreNode,
  ): Promise<MarrowDataNode[]> {
    let result: SavedChildrenResult;
    try {
      result = await client.sendRequest<SavedChildrenResult>(REQUEST_SAVED_CHILDREN, {
        path: node.path,
      });
    } catch {
      return [];
    }
    if (!result.available) {
      return [unavailableNode()];
    }
    const nodes: MarrowDataNode[] = result.children.map((child) =>
      childToNode(node.path, child),
    );
    if (result.more) {
      nodes.push({ kind: "more", label: "…more" });
    }
    return nodes;
  }
}

function unavailableNode(): PlaceholderNode {
  return { kind: "unavailable", label: "store unavailable" };
}

function unavailablePathMetadataNode(): PlaceholderNode {
  return { kind: "unavailable", label: "path metadata unavailable" };
}

// Build the inline description from a savedGet result: the value, with the type
// in parentheses when present.
function describeValue(result: SavedGetResult): string | undefined {
  if (result.value === undefined) {
    return result.type !== undefined ? `(${result.type})` : undefined;
  }
  return result.type !== undefined ? `${result.value} (${result.type})` : result.value;
}

function describeSchema(node: StoreNode, includeType: boolean): string | undefined {
  const detail = node.detail ?? node.role;
  if (!includeType || node.type === undefined) {
    return detail;
  }
  return detail === undefined ? `(${node.type})` : `${detail} (${node.type})`;
}

function joinDescriptions(
  primary: string | undefined,
  secondary: string | undefined,
): string | undefined {
  if (primary === undefined) {
    return secondary;
  }
  return secondary === undefined ? primary : `${primary} - ${secondary}`;
}
