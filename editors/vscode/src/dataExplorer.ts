import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

interface SavedRootsResult {
  available: boolean;
  roots: string[];
}

const REQUEST_SAVED_ROOTS = "marrow/savedRoots";

type MarrowDataNode = RootNode | PlaceholderNode;

interface RootNode {
  readonly kind: "root";
  readonly label: string;
}

interface PlaceholderNode {
  readonly kind: "unavailable";
  readonly label: string;
}

function isRootNode(node: MarrowDataNode): node is RootNode {
  return node.kind === "root";
}

export class MarrowDataProvider implements vscode.TreeDataProvider<MarrowDataNode> {
  private readonly emitter = new vscode.EventEmitter<MarrowDataNode | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  private client: LanguageClient | undefined;

  setClient(client: LanguageClient | undefined): void {
    this.client = client;
    this.refresh();
  }

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(node: MarrowDataNode): vscode.TreeItem {
    const item = new vscode.TreeItem(node.label, vscode.TreeItemCollapsibleState.None);
    item.contextValue = node.kind === "root" ? "marrowDataRoot" : node.kind;
    if (!isRootNode(node)) {
      item.iconPath = new vscode.ThemeIcon("circle-slash");
    }
    return item;
  }

  async getChildren(node?: MarrowDataNode): Promise<MarrowDataNode[]> {
    if (node !== undefined) {
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
    return result.roots.map((root): RootNode => ({ kind: "root", label: root }));
  }
}

function unavailableNode(): PlaceholderNode {
  return { kind: "unavailable", label: "store unavailable" };
}
