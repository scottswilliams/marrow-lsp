import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";
import {
  dataViewBoundaryFromWire,
  sameBoundary,
  type DataViewBoundary,
  type StoreSnapshot,
} from "./dataViewBoundary";

interface SavedRootsResult {
  available: boolean;
  roots: SavedRootView[];
  store_snapshot: StoreSnapshot | null;
  data_view_boundary?: DataViewBoundary;
}

type DataRootSegment = { readonly kind: "root"; readonly store_catalog_id: string };

type DataPathSegment =
  | DataRootSegment
  | { readonly kind: "field"; readonly member_catalog_id: string }
  | { readonly kind: "layer"; readonly member_catalog_id: string }
  | { readonly kind: "key"; readonly value: DataKey };

type DataKey =
  | { readonly kind: "int"; readonly value: number }
  | { readonly kind: "bool"; readonly value: boolean }
  | { readonly kind: "string"; readonly value: string }
  | { readonly kind: "date"; readonly value: number }
  | { readonly kind: "instant"; readonly value: string }
  | { readonly kind: "duration"; readonly value: string }
  | { readonly kind: "bytes"; readonly value: number[] };

interface DataChildView {
  readonly segment: DataPathSegment;
  readonly label: string;
}

interface SavedRootView {
  readonly segment: DataRootSegment;
  readonly label: string;
}

interface DataChildViewsResult {
  readonly available: boolean;
  readonly children: DataChildView[];
  readonly truncated: boolean;
  readonly cursor: DataKey | null;
  readonly store_snapshot: StoreSnapshot | null;
  readonly data_view_boundary?: DataViewBoundary;
  readonly error?: unknown;
}

interface DataReadRequest {
  readonly segments: DataPathSegment[];
  readonly preview_limit?: number;
}

interface DataReadResult {
  readonly available: boolean;
  readonly presence: "absent" | "value_only" | "children_only";
  readonly value?: string;
  readonly value_truncated: boolean;
  readonly store_snapshot: StoreSnapshot | null;
  readonly data_view_boundary?: DataViewBoundary;
  readonly error?: unknown;
}

const REQUEST_SAVED_ROOTS = "marrow/savedRoots";
const REQUEST_DATA_CHILDREN = "marrow/dataChildren";
const REQUEST_DATA_READ = "marrow/dataRead";
const DATA_PAGE_LIMIT = 200;
const DATA_VALUE_PREVIEW_LIMIT = 2048;

export type SavedResourceNode =
  | SavedRootNode
  | SavedSegmentNode
  | SavedValueNode
  | SavedMoreNode
  | SavedPlaceholderNode;

export interface SavedRootNode {
  readonly kind: "root";
  readonly label: string;
  readonly segment: DataRootSegment;
  readonly boundary: DataViewBoundary;
}

interface SavedSegmentNode {
  readonly kind: "segment";
  readonly label: string;
  readonly segments: DataPathSegment[];
  readonly boundary: DataViewBoundary;
}

interface SavedValueNode {
  readonly kind: "value";
  readonly label: string;
  readonly valueTruncated: boolean;
}

interface SavedMoreNode {
  readonly kind: "more";
  readonly label: string;
  readonly segments: DataPathSegment[];
  readonly cursor: DataKey;
  readonly boundary: DataViewBoundary;
}

interface SavedPlaceholderNode {
  readonly kind: "unavailable" | "blocked" | "absent" | "truncated" | "changed";
  readonly label: string;
}

function canExpand(node: SavedResourceNode): boolean {
  return node.kind === "root" || node.kind === "segment" || node.kind === "more";
}

export class SavedResourceProvider implements vscode.TreeDataProvider<SavedResourceNode> {
  private readonly emitter = new vscode.EventEmitter<SavedResourceNode | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  private client: LanguageClient | undefined;

  setClient(client: LanguageClient | undefined): void {
    this.client = client;
    this.refresh();
  }

  refresh(): void {
    this.emitter.fire(undefined);
  }

  async rootNodes(): Promise<SavedRootNode[]> {
    const loaded = await this.loadRootNodes();
    return loaded.kind === "roots" ? loaded.roots : [];
  }

  getTreeItem(node: SavedResourceNode): vscode.TreeItem {
    const item = new vscode.TreeItem(
      node.label,
      canExpand(node)
        ? vscode.TreeItemCollapsibleState.Collapsed
        : vscode.TreeItemCollapsibleState.None,
    );
    item.contextValue = node.kind === "root" ? "marrowDataRoot" : node.kind;
    if (!canExpand(node) && node.kind !== "value") {
      item.iconPath = new vscode.ThemeIcon("circle-slash");
    }
    if (node.kind === "value" && node.valueTruncated) {
      item.description = "truncated";
    }
    return item;
  }

  async getChildren(node?: SavedResourceNode): Promise<SavedResourceNode[]> {
    if (node !== undefined) {
      if (node.kind === "root") {
        return this.getDataChildren([node.segment], null, node.boundary);
      }
      if (node.kind === "segment") {
        return this.getSegmentChildren(node);
      }
      if (node.kind === "more") {
        return this.getDataChildren(node.segments, node.cursor, node.boundary);
      }
      return [];
    }
    const loaded = await this.loadRootNodes();
    if (loaded.kind === "noClient") {
      return [];
    }
    if (loaded.kind === "unavailable") {
      return [unavailableNode()];
    }
    return loaded.roots;
  }

  private async loadRootNodes(): Promise<
    | { readonly kind: "noClient" }
    | { readonly kind: "unavailable" }
    | { readonly kind: "roots"; readonly roots: SavedRootNode[] }
  > {
    const client = this.client;
    if (client === undefined) {
      return { kind: "noClient" };
    }
    let result: SavedRootsResult;
    try {
      result = await client.sendRequest<SavedRootsResult>(REQUEST_SAVED_ROOTS);
    } catch {
      return { kind: "unavailable" };
    }
    if (!result.available) {
      return { kind: "unavailable" };
    }
    const boundary = dataViewBoundaryFromWire(result.data_view_boundary);
    if (boundary === undefined) {
      return { kind: "unavailable" };
    }
    return {
      kind: "roots",
      roots: result.roots.map(
        (root): SavedRootNode => ({
          kind: "root",
          label: root.label,
          segment: root.segment,
          boundary,
        }),
      ),
    };
  }

  private async getDataChildren(
    segments: DataPathSegment[],
    cursor: DataKey | null,
    expectedBoundary: DataViewBoundary,
  ): Promise<SavedResourceNode[]> {
    const client = this.client;
    if (client === undefined) {
      return [];
    }
    let result: DataChildViewsResult;
    try {
      result = await client.sendRequest<DataChildViewsResult>(REQUEST_DATA_CHILDREN, {
        segments,
        limit: DATA_PAGE_LIMIT,
        cursor,
      });
    } catch {
      return [unavailableNode()];
    }
    if (!result.available) {
      return [unavailableNode()];
    }
    const boundary = dataViewBoundaryFromWire(result.data_view_boundary);
    if (boundary === undefined || !sameBoundary(expectedBoundary, boundary)) {
      return [changedNode()];
    }
    if (result.error !== undefined) {
      return [{ kind: "blocked", label: "path unavailable" }];
    }
    const nodes: SavedResourceNode[] = result.children.map((child): SavedSegmentNode => ({
      kind: "segment",
      label: child.label,
      segments: [...segments, child.segment],
      boundary,
    }));
    if (result.truncated) {
      if (result.cursor !== null) {
        nodes.push({
          kind: "more",
          label: "more children",
          segments: [...segments],
          cursor: result.cursor,
          boundary,
        });
      } else {
        nodes.push({ kind: "truncated", label: "additional children unavailable" });
      }
    }
    return nodes;
  }

  private async getSegmentChildren(node: SavedSegmentNode): Promise<SavedResourceNode[]> {
    const read = await this.readData(node.segments, node.boundary);
    if (read !== undefined) {
      return read;
    }
    return this.getDataChildren(node.segments, null, node.boundary);
  }

  private async readData(
    segments: DataPathSegment[],
    expectedBoundary: DataViewBoundary,
  ): Promise<SavedResourceNode[] | undefined> {
    const client = this.client;
    if (client === undefined) {
      return [];
    }
    let result: DataReadResult;
    try {
      const request: DataReadRequest = {
        segments,
        preview_limit: DATA_VALUE_PREVIEW_LIMIT,
      };
      result = await client.sendRequest<DataReadResult>(REQUEST_DATA_READ, request);
    } catch {
      return [unavailableNode()];
    }
    if (!result.available) {
      return [unavailableNode()];
    }
    const boundary = dataViewBoundaryFromWire(result.data_view_boundary);
    if (boundary === undefined || !sameBoundary(expectedBoundary, boundary)) {
      return [changedNode()];
    }
    if (result.error !== undefined) {
      return [{ kind: "blocked", label: "path unavailable" }];
    }
    if (result.value !== undefined) {
      return [{ kind: "value", label: result.value, valueTruncated: result.value_truncated }];
    }
    if (result.presence === "absent") {
      return [{ kind: "absent", label: "absent" }];
    }
    return undefined;
  }
}

function unavailableNode(): SavedPlaceholderNode {
  return { kind: "unavailable", label: "store unavailable" };
}

function changedNode(): SavedPlaceholderNode {
  return { kind: "changed", label: "data changed; refresh" };
}
