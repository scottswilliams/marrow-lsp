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
import { MarrowDataIntegrity } from "./dataIntegrity";

// The one shared language client for the whole extension. Every transport-facing
// feature (diagnostics, the Data Explorer) speaks through this single instance.
let client: LanguageClient | undefined;

const EXE_SUFFIX = process.platform === "win32" ? ".exe" : "";
const SERVER_BINARY = `marrow-lsp${EXE_SUFFIX}`;
const DAP_BINARY = `marrow-dap${EXE_SUFFIX}`;

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

  // The integrity check is an advisory report wired up only with the client
  // running, so its request always hits a live server. It shares the single
  // client instance and owns a dedicated output channel disposed below.
  const dataIntegrity = new MarrowDataIntegrity(client);

  context.subscriptions.push(
    dataIntegrity,
    vscode.window.registerTreeDataProvider("marrowData", dataProvider),
    vscode.commands.registerCommand("marrow.refreshData", () => {
      dataProvider.refresh();
    }),
    vscode.commands.registerCommand("marrow.checkDataIntegrity", () => {
      void dataIntegrity.run();
    }),
    // A saved store change is announced by a write to marrow.json; reflect it
    // in the tree automatically so live values stay current without a manual
    // refresh.
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (path.basename(document.fileName) === "marrow.json") {
        dataProvider.refresh();
      }
    }),
    // F5 debugging: launch the bundled marrow-dap adapter over stdio and fill
    // the project/entry defaults the user left out of their launch config.
    vscode.debug.registerDebugAdapterDescriptorFactory(
      "marrow",
      new MarrowDebugAdapterFactory(context),
    ),
    vscode.debug.registerDebugConfigurationProvider(
      "marrow",
      new MarrowDebugConfigurationProvider(),
    ),
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
  return resolveBinaryPath(context, "server.path", SERVER_BINARY);
}

// The debug adapter resolves exactly like the language server: the
// `marrow.dap.path` setting, then the bundled binary, then the dev build.
function resolveDapPath(context: vscode.ExtensionContext): string | undefined {
  return resolveBinaryPath(context, "dap.path", DAP_BINARY);
}

// Resolve a bundled transport binary by config override, then bundled copy,
// then local dev build. The first one that exists on disk wins so a configured
// override is always authoritative.
function resolveBinaryPath(
  context: vscode.ExtensionContext,
  setting: string,
  binary: string,
): string | undefined {
  const configured = vscode.workspace
    .getConfiguration("marrow")
    .get<string>(setting)
    ?.trim();
  if (configured) {
    return configured;
  }

  const bundled = path.join(context.extensionPath, "server", binary);
  if (fs.existsSync(bundled)) {
    return bundled;
  }

  const dev = path.join(context.extensionPath, "..", "..", "target", "debug", binary);
  if (fs.existsSync(dev)) {
    return dev;
  }

  return undefined;
}

// Launches the marrow-dap adapter as a child process speaking DAP over stdio.
class MarrowDebugAdapterFactory implements vscode.DebugAdapterDescriptorFactory {
  constructor(private readonly context: vscode.ExtensionContext) {}

  createDebugAdapterDescriptor(): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    const dapPath = resolveDapPath(this.context);
    if (dapPath === undefined) {
      void vscode.window.showErrorMessage(
        "Marrow: could not find the marrow-dap debug adapter binary. " +
          "Set `marrow.dap.path` to its absolute location, bundle it at " +
          "`server/marrow-dap`, or build the dev binary at `target/debug/marrow-dap`.",
      );
      return undefined;
    }
    return new vscode.DebugAdapterExecutable(dapPath, []);
  }
}

// Fills the defaults a user omits from a launch config: the project directory
// (the marrow.json folder or the workspace folder) and the entry file (the
// active .mw editor). The adapter interprets the project and entry; this only
// supplies sensible launch defaults.
class MarrowDebugConfigurationProvider implements vscode.DebugConfigurationProvider {
  resolveDebugConfiguration(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): vscode.ProviderResult<vscode.DebugConfiguration> {
    // A bare F5 with no launch.json arrives as an empty config; seed it so the
    // active .mw file is debugged without the user authoring a launch entry.
    if (!config.type && !config.request && !config.name) {
      config.type = "marrow";
      config.request = "launch";
      config.name = "Debug Marrow entry";
    }

    if (config.entry === undefined) {
      const active = vscode.window.activeTextEditor?.document;
      if (active?.languageId === "marrow") {
        config.entry = active.uri.fsPath;
      }
    }

    if (config.project === undefined) {
      config.project = defaultProject(folder, config.entry);
    }

    return config;
  }
}

// The project is the directory holding marrow.json, searched upward from the
// entry file; falling back to the workspace folder when none is found.
function defaultProject(
  folder: vscode.WorkspaceFolder | undefined,
  entry: string | undefined,
): string | undefined {
  if (typeof entry === "string" && entry.length > 0) {
    let dir = path.dirname(entry);
    while (true) {
      if (fs.existsSync(path.join(dir, "marrow.json"))) {
        return dir;
      }
      const parent = path.dirname(dir);
      if (parent === dir) {
        break;
      }
      dir = parent;
    }
  }
  return folder?.uri.fsPath;
}
