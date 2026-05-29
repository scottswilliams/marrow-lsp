import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

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
