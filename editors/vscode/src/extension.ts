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

let client: LanguageClient | undefined;

const EXE_SUFFIX = process.platform === "win32" ? ".exe" : "";
const SERVER_BINARY = `marrow-lsp${EXE_SUFFIX}`;
const DAP_BINARY = `marrow-dap${EXE_SUFFIX}`;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const dataProvider = new MarrowDataProvider();
  let dataIntegrity: MarrowDataIntegrity | undefined;

  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("marrowData", dataProvider),
    vscode.commands.registerCommand("marrow.refreshData", () => {
      dataProvider.refresh();
    }),
    vscode.commands.registerCommand("marrow.checkDataIntegrity", () => {
      if (dataIntegrity === undefined) {
        void vscode.window.showErrorMessage("Marrow: language server is not running.");
        return;
      }
      void dataIntegrity.run();
    }),
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (path.basename(document.fileName) === "marrow.json") {
        dataProvider.refresh();
      }
    }),
    vscode.debug.registerDebugAdapterDescriptorFactory(
      "marrow",
      new MarrowDebugAdapterFactory(context),
    ),
    vscode.debug.registerDebugConfigurationProvider(
      "marrow",
      new MarrowDebugConfigurationProvider(),
    ),
  );

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
    initializationOptions: {
      "marrow.liveData": vscode.workspace
        .getConfiguration("marrow")
        .get<boolean>("liveData", false),
    },
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

  try {
    await client.start();
  } catch (error) {
    client = undefined;
    void vscode.window.showErrorMessage(`Marrow: failed to start language server: ${error}`);
    return;
  }

  dataProvider.setClient(client);

  dataIntegrity = new MarrowDataIntegrity(client);

  context.subscriptions.push(dataIntegrity);
}

export async function deactivate(): Promise<void> {
  if (client === undefined) {
    return;
  }
  await client.stop();
  client = undefined;
}

function resolveServerPath(context: vscode.ExtensionContext): string | undefined {
  return resolveBinaryPath(context, "server.path", SERVER_BINARY);
}

function resolveDapPath(context: vscode.ExtensionContext): string | undefined {
  return resolveBinaryPath(context, "dap.path", DAP_BINARY);
}

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
    return isUsableBinary(configured) ? configured : undefined;
  }

  const bundled = path.join(context.extensionPath, "server", binary);
  if (isUsableBinary(bundled)) {
    return bundled;
  }

  const dev = path.join(context.extensionPath, "..", "..", "target", "debug", binary);
  if (context.extensionMode !== vscode.ExtensionMode.Production && isUsableBinary(dev)) {
    return dev;
  }

  return undefined;
}

function isUsableBinary(file: string): boolean {
  if (!path.isAbsolute(file)) {
    return false;
  }
  try {
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

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

class MarrowDebugConfigurationProvider implements vscode.DebugConfigurationProvider {
  resolveDebugConfiguration(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): vscode.ProviderResult<vscode.DebugConfiguration> {
    if (!config.type && !config.request && !config.name) {
      config.type = "marrow";
      config.request = "launch";
      config.name = "Debug Marrow entry";
    }

    const active = vscode.window.activeTextEditor?.document;
    const activeMarrowFile = active?.languageId === "marrow" ? active.uri.fsPath : undefined;

    if (config.project === undefined) {
      config.project = defaultProject(folder, activeMarrowFile);
    }

    return config;
  }
}

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
