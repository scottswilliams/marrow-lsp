import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

interface SavedRootsResult {
  available: boolean;
  roots: SavedRootView[];
  store_snapshot: StoreSnapshot | null;
}

type StoreSnapshot = JsonObject;
type JsonObject = { readonly [key: string]: JsonValue };
type JsonValue = string | number | boolean | null | JsonObject | readonly JsonValue[];

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
  readonly snapshot: StoreSnapshot | null;
}

interface SavedSegmentNode {
  readonly kind: "segment";
  readonly label: string;
  readonly segments: DataPathSegment[];
  readonly snapshot: StoreSnapshot | null;
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
  readonly snapshot: StoreSnapshot | null;
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
    return result.roots.map(
      (root): SavedRootNode => ({
        kind: "root",
        label: root.label,
        segment: root.segment,
        snapshot: result.store_snapshot,
      }),
    );
  }

  private async getDataChildren(
    segments: DataPathSegment[],
    cursor: DataKey | null,
    expectedSnapshot: StoreSnapshot | null,
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
    if (!sameSnapshot(expectedSnapshot, result.store_snapshot)) {
      return [changedNode()];
    }
    if (result.error !== undefined) {
      return [{ kind: "blocked", label: "path unavailable" }];
    }
    const nodes: SavedResourceNode[] = result.children.map((child): SavedSegmentNode => ({
      kind: "segment",
      label: child.label,
      segments: [...segments, child.segment],
      snapshot: result.store_snapshot,
    }));
    if (result.truncated) {
      if (result.cursor !== null) {
        nodes.push({
          kind: "more",
          label: "more children",
          segments: [...segments],
          cursor: result.cursor,
          snapshot: result.store_snapshot,
        });
      } else {
        nodes.push({ kind: "truncated", label: "additional children unavailable" });
      }
    }
    return nodes;
  }

  private async getSegmentChildren(node: SavedSegmentNode): Promise<SavedResourceNode[]> {
    if (readsValue(node)) {
      const read = await this.readData(node.segments, node.snapshot);
      if (read !== undefined) {
        return read;
      }
    }
    return this.getDataChildren(node.segments, null, node.snapshot);
  }

  private async readData(
    segments: DataPathSegment[],
    expectedSnapshot: StoreSnapshot | null,
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
    if (!sameSnapshot(expectedSnapshot, result.store_snapshot)) {
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

function sameSnapshot(left: StoreSnapshot | null, right: StoreSnapshot | null): boolean {
  return sameJson(left, right);
}

function sameJson(left: JsonValue, right: JsonValue): boolean {
  if (left === right) {
    return true;
  }
  if (left === null || right === null) {
    return false;
  }
  if (isJsonArray(left) || isJsonArray(right)) {
    return sameJsonArray(left, right);
  }
  if (isJsonObject(left) && isJsonObject(right)) {
    return sameJsonObject(left, right);
  }
  return false;
}

function isJsonArray(value: JsonValue): value is readonly JsonValue[] {
  return Array.isArray(value);
}

function sameJsonArray(left: JsonValue, right: JsonValue): boolean {
  if (!isJsonArray(left) || !isJsonArray(right) || left.length !== right.length) {
    return false;
  }
  return left.every((value, index) => sameJson(value, right[index]));
}

function isJsonObject(value: JsonValue): value is JsonObject {
  return typeof value === "object" && value !== null && !isJsonArray(value);
}

function sameJsonObject(left: JsonObject, right: JsonObject): boolean {
  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  if (leftKeys.length !== rightKeys.length) {
    return false;
  }
  return leftKeys.every((key) =>
    Object.hasOwn(right, key) && sameJson(left[key], right[key]),
  );
}

function readsValue(node: SavedSegmentNode): boolean {
  const last = node.segments[node.segments.length - 1];
  return last?.kind === "field";
}
