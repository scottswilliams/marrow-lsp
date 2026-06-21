import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

const DATA_GENERATION_PROFILE_VERSION = "data.generation.v1";

interface SavedRootsResult {
  available: boolean;
  roots: SavedRootView[];
  store_snapshot: StoreSnapshot | null;
}

type StoreSnapshot = DataGeneration;

interface DataGeneration {
  readonly profile_version: typeof DATA_GENERATION_PROFILE_VERSION;
  readonly store_uid: string | null;
  readonly catalog_digest: string | null;
  readonly commit: DataGenerationCommit | null;
  readonly open_transaction: DataGenerationTransaction | null;
  readonly checked_source_digest: string;
}

interface DataGenerationCommit {
  readonly commit_id: number;
  readonly catalog_epoch: number;
  readonly source_digest: string;
  readonly layout_epoch: number;
  readonly engine_profile_digest: string;
}

interface DataGenerationTransaction {
  readonly depth: number;
}

type DataRootSegment = { readonly kind: "root"; readonly value: string };

type DataPathSegment =
  | DataRootSegment
  | { readonly kind: "field"; readonly value: string }
  | { readonly kind: "layer"; readonly value: string }
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

interface DataChildrenResult {
  readonly available: boolean;
  readonly children: DataChildView[];
  readonly truncated: boolean;
  readonly cursor: DataKey | null;
  readonly store_snapshot: StoreSnapshot | null;
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
  readonly error?: unknown;
}

const REQUEST_SAVED_ROOTS = "marrow/savedRoots";
const REQUEST_DATA_CHILDREN = "marrow/dataChildren";
const REQUEST_DATA_READ = "marrow/dataRead";
const DATA_PAGE_LIMIT = 200;
const DATA_VALUE_PREVIEW_LIMIT = 2048;

type SavedResourceNode =
  | SavedRootNode
  | SavedSegmentNode
  | SavedValueNode
  | SavedMoreNode
  | SavedPlaceholderNode;

interface SavedRootNode {
  readonly kind: "root";
  readonly label: string;
  readonly segment: DataRootSegment;
  readonly snapshot: StoreSnapshot;
}

interface SavedSegmentNode {
  readonly kind: "segment";
  readonly label: string;
  readonly segments: DataPathSegment[];
  readonly snapshot: StoreSnapshot;
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
  readonly snapshot: StoreSnapshot;
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
        return this.getDataChildren([node.segment], null, node.snapshot);
      }
      if (node.kind === "segment") {
        return this.getSegmentChildren(node);
      }
      if (node.kind === "more") {
        return this.getDataChildren(node.segments, node.cursor, node.snapshot);
      }
      return [];
    }
    const client = this.client;
    if (client === undefined) {
      return [];
    }
    let result: SavedRootsResult;
    try {
      result = await client.sendRequest<SavedRootsResult>(REQUEST_SAVED_ROOTS);
    } catch {
      return [unavailableNode()];
    }
    if (!result.available) {
      return [unavailableNode()];
    }
    const snapshot = storeSnapshotFromWire(result.store_snapshot);
    if (snapshot === undefined) {
      return [unavailableNode()];
    }
    return result.roots.map(
      (root): SavedRootNode => ({
        kind: "root",
        label: root.label,
        segment: root.segment,
        snapshot,
      }),
    );
  }

  private async getDataChildren(
    segments: DataPathSegment[],
    cursor: DataKey | null,
    expectedSnapshot: StoreSnapshot,
  ): Promise<SavedResourceNode[]> {
    const client = this.client;
    if (client === undefined) {
      return [];
    }
    let result: DataChildrenResult;
    try {
      result = await client.sendRequest<DataChildrenResult>(REQUEST_DATA_CHILDREN, {
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
    const snapshot = storeSnapshotFromWire(result.store_snapshot);
    if (snapshot === undefined || !sameSnapshot(expectedSnapshot, snapshot)) {
      return [changedNode()];
    }
    if (result.error !== undefined) {
      return [{ kind: "blocked", label: "path unavailable" }];
    }
    const nodes: SavedResourceNode[] = result.children.map((child): SavedSegmentNode => ({
      kind: "segment",
      label: child.label,
      segments: [...segments, child.segment],
      snapshot,
    }));
    if (result.truncated) {
      if (result.cursor !== null) {
        nodes.push({
          kind: "more",
          label: "more children",
          segments: [...segments],
          cursor: result.cursor,
          snapshot,
        });
      } else {
        nodes.push({ kind: "truncated", label: "additional children unavailable" });
      }
    }
    return nodes;
  }

  private async getSegmentChildren(node: SavedSegmentNode): Promise<SavedResourceNode[]> {
    const read = await this.readData(node.segments, node.snapshot);
    if (read !== undefined) {
      return read;
    }
    return this.getDataChildren(node.segments, null, node.snapshot);
  }

  private async readData(
    segments: DataPathSegment[],
    expectedSnapshot: StoreSnapshot,
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
    const snapshot = storeSnapshotFromWire(result.store_snapshot);
    if (snapshot === undefined || !sameSnapshot(expectedSnapshot, snapshot)) {
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

function storeSnapshotFromWire(value: unknown): StoreSnapshot | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  if (value.profile_version !== DATA_GENERATION_PROFILE_VERSION) {
    return undefined;
  }
  if (
    !isStringOrNull(value.store_uid) ||
    !isStringOrNull(value.catalog_digest) ||
    typeof value.checked_source_digest !== "string"
  ) {
    return undefined;
  }
  const commit = generationCommitFromWire(value.commit);
  if (commit === undefined) {
    return undefined;
  }
  const transaction = generationTransactionFromWire(value.open_transaction);
  if (transaction === undefined) {
    return undefined;
  }
  return {
    profile_version: DATA_GENERATION_PROFILE_VERSION,
    store_uid: value.store_uid,
    catalog_digest: value.catalog_digest,
    commit,
    open_transaction: transaction,
    checked_source_digest: value.checked_source_digest,
  };
}

function generationCommitFromWire(value: unknown): DataGenerationCommit | null | undefined {
  if (value === null) {
    return null;
  }
  if (!isRecord(value)) {
    return undefined;
  }
  if (
    !isSafeInteger(value.commit_id) ||
    !isSafeInteger(value.catalog_epoch) ||
    typeof value.source_digest !== "string" ||
    !isSafeInteger(value.layout_epoch) ||
    typeof value.engine_profile_digest !== "string"
  ) {
    return undefined;
  }
  return {
    commit_id: value.commit_id,
    catalog_epoch: value.catalog_epoch,
    source_digest: value.source_digest,
    layout_epoch: value.layout_epoch,
    engine_profile_digest: value.engine_profile_digest,
  };
}

function generationTransactionFromWire(
  value: unknown,
): DataGenerationTransaction | null | undefined {
  if (value === null) {
    return null;
  }
  if (!isRecord(value) || !isSafeInteger(value.depth)) {
    return undefined;
  }
  return { depth: value.depth };
}

function isSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value);
}

function isStringOrNull(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sameSnapshot(left: StoreSnapshot, right: StoreSnapshot): boolean {
  if (left === right) {
    return true;
  }
  return (
    left.profile_version === right.profile_version &&
    left.store_uid === right.store_uid &&
    left.catalog_digest === right.catalog_digest &&
    sameCommit(left.commit, right.commit) &&
    sameTransaction(left.open_transaction, right.open_transaction) &&
    left.checked_source_digest === right.checked_source_digest
  );
}

function sameCommit(
  left: DataGenerationCommit | null,
  right: DataGenerationCommit | null,
): boolean {
  if (left === right) {
    return true;
  }
  if (left === null || right === null) {
    return false;
  }
  return (
    left.commit_id === right.commit_id &&
    left.catalog_epoch === right.catalog_epoch &&
    left.source_digest === right.source_digest &&
    left.layout_epoch === right.layout_epoch &&
    left.engine_profile_digest === right.engine_profile_digest
  );
}

function sameTransaction(
  left: DataGenerationTransaction | null,
  right: DataGenerationTransaction | null,
): boolean {
  if (left === right) {
    return true;
  }
  if (left === null || right === null) {
    return false;
  }
  return left.depth === right.depth;
}
