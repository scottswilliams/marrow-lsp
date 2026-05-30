import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";
import { MarrowDataProvider } from "./dataExplorer";

// The one shared language client for the whole extension. Every transport-facing
// feature (diagnostics, the Data Explorer) speaks through this single instance.
let client: LanguageClient | undefined;

const SERVER_BINARY = process.platform === "win32" ? "marrow-lsp.exe" : "marrow-lsp";

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const serverPath = resolveServerPath(context);
  if (serverPath === undefined) {
    void vscode.window.showErrorMessage(
      "Marrow: could not find the marrow-lsp server binary. " +
        "Set `marrow.server.path` to its absolute location, bundle it at " +
        "`server/marrow-lsp`, or build the dev binary at `target/debug/marrow-lsp`.",
    );
    return;
  }

  const serverOptions: ServerOptions = {
    run: { command: serverPath, transport: TransportKind.stdio },
    debug: { command: serverPath, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ language: "marrow" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/marrow.json"),
    },
  };

  client = new LanguageClient(
    "marrow",
    "Marrow Language Server",
    serverOptions,
    clientOptions,
  );

  await client.start();

  // The Data Explorer is only wired up once the client is running, so every
  // request it issues hits a live server. It shares the single client instance.
  const dataProvider = new MarrowDataProvider();
  dataProvider.setClient(client);

  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("marrowData", dataProvider),
    vscode.commands.registerCommand("marrow.refreshData", () => {
      dataProvider.refresh();
    }),
    // A saved store change is announced by a write to marrow.json; reflect it
    // in the tree automatically so live values stay current without a manual
    // refresh.
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (path.basename(document.fileName) === "marrow.json") {
        dataProvider.refresh();
      }
    }),
  );
}

export async function deactivate(): Promise<void> {
  if (client === undefined) {
    return;
  }
  await client.stop();
  client = undefined;
}

// The server path comes from, in order: the `marrow.server.path` setting, the
// binary bundled with the extension, then the local dev build. The first one
// that exists on disk wins so a configured override is always authoritative.
function resolveServerPath(context: vscode.ExtensionContext): string | undefined {
  const configured = vscode.workspace
    .getConfiguration("marrow")
    .get<string>("server.path")
    ?.trim();
  if (configured) {
    return configured;
  }

  const bundled = path.join(context.extensionPath, "server", SERVER_BINARY);
  if (fs.existsSync(bundled)) {
    return bundled;
  }

  const dev = path.join(context.extensionPath, "..", "..", "target", "debug", SERVER_BINARY);
  if (fs.existsSync(dev)) {
    return dev;
  }

  return undefined;
}
