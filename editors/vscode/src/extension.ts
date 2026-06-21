import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
  FileChangeType,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";
import { SavedResourceProvider } from "./savedResourceInspector";
import { MarrowDataIntegrity } from "./dataIntegrity";

let client: LanguageClient | undefined;

const EXE_SUFFIX = process.platform === "win32" ? ".exe" : "";
const SERVER_BINARY = `marrow-lsp${EXE_SUFFIX}`;
const DAP_BINARY = `marrow-dap${EXE_SUFFIX}`;
const REQUEST_DATA_WATCH_TARGETS = "marrow/dataWatchTargets";
const NOTIFICATION_DID_CHANGE_WATCHED_FILES = "workspace/didChangeWatchedFiles";

interface SavedDataWatchTargets {
  readonly paths: string[];
}

interface DataWatchTargetClient {
  sendRequest<T>(method: string): Thenable<T>;
  sendNotification(method: string, params: unknown): Thenable<void>;
}

interface RefreshableSavedResourceProvider {
  refresh(): void;
}

export class SavedDataWatchTargetRegistry implements vscode.Disposable {
  private watchers: vscode.FileSystemWatcher[] = [];
  private refreshSeq = 0;
  private disposed = false;

  constructor(
    private readonly client: DataWatchTargetClient,
    private readonly provider: RefreshableSavedResourceProvider,
  ) {}

  async refresh(): Promise<void> {
    const seq = ++this.refreshSeq;
    let result: SavedDataWatchTargets;
    try {
      result = await this.client.sendRequest<SavedDataWatchTargets>(REQUEST_DATA_WATCH_TARGETS);
    } catch {
      return;
    }
    if (this.disposed || seq !== this.refreshSeq) {
      return;
    }
    const nextWatchers: vscode.FileSystemWatcher[] = [];
    for (const target of result.paths) {
      const fsPath = watchTargetFsPath(target);
      if (fsPath === undefined) {
        continue;
      }
      const watcher = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(path.dirname(fsPath), path.basename(fsPath)),
      );
      watcher.onDidChange(() => void this.handleTargetEvent(fsPath, FileChangeType.Changed));
      watcher.onDidCreate(() => void this.handleTargetEvent(fsPath, FileChangeType.Created));
      watcher.onDidDelete(() => void this.handleTargetEvent(fsPath, FileChangeType.Deleted));
      nextWatchers.push(watcher);
    }
    if (this.disposed || seq !== this.refreshSeq) {
      for (const watcher of nextWatchers) {
        watcher.dispose();
      }
      return;
    }
    this.disposeWatchers();
    this.watchers = nextWatchers;
  }

  dispose(): void {
    this.disposed = true;
    this.refreshSeq += 1;
    this.disposeWatchers();
  }

  private disposeWatchers(): void {
    for (const watcher of this.watchers) {
      watcher.dispose();
    }
    this.watchers = [];
  }

  private async handleTargetEvent(fsPath: string, type: FileChangeType): Promise<void> {
    try {
      await this.client.sendNotification(NOTIFICATION_DID_CHANGE_WATCHED_FILES, {
        changes: [{ uri: vscode.Uri.file(fsPath).toString(), type }],
      });
    } catch {
      // A failed invalidation notification must not leave the inspector stale forever.
    }
    this.provider.refresh();
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const dataProvider = new SavedResourceProvider();
  let dataIntegrity: MarrowDataIntegrity | undefined;
  let dataWatchTargets: SavedDataWatchTargetRegistry | undefined;
  const refreshWatchTargets = () => {
    void dataWatchTargets?.refresh();
  };

  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("marrowData", dataProvider),
    vscode.commands.registerCommand("marrow.refreshData", () => {
      void dataWatchTargets?.refresh();
      dataProvider.refresh();
    }),
    vscode.commands.registerCommand("marrow.checkDataIntegrity", () => {
      if (dataIntegrity === undefined) {
        void vscode.window.showErrorMessage("Marrow: language server is not running.");
        return;
      }
      void dataIntegrity.run();
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
    middleware: {
      handleDiagnostics(uri, diagnostics, next) {
        next(uri, diagnostics);
        refreshWatchTargets();
        dataProvider.refresh();
      },
    },
    synchronize: {
      fileEvents: [
        vscode.workspace.createFileSystemWatcher("**/marrow.json"),
        vscode.workspace.createFileSystemWatcher("**/*.mw"),
      ],
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
  dataWatchTargets = new SavedDataWatchTargetRegistry(client, dataProvider);
  await dataWatchTargets.refresh();

  const projectConfigWatcher = vscode.workspace.createFileSystemWatcher("**/marrow.json");
  projectConfigWatcher.onDidChange(refreshWatchTargets);
  projectConfigWatcher.onDidCreate(refreshWatchTargets);
  projectConfigWatcher.onDidDelete(refreshWatchTargets);

  context.subscriptions.push(
    dataIntegrity,
    dataWatchTargets,
    projectConfigWatcher,
    vscode.window.onDidChangeActiveTextEditor(refreshWatchTargets),
    vscode.workspace.onDidOpenTextDocument((document) => {
      if (document.languageId === "marrow") {
        refreshWatchTargets();
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

function watchTargetFsPath(target: string): string | undefined {
  if (path.isAbsolute(target)) {
    return target;
  }
  return undefined;
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
      config.name = "Debug default Marrow entry";
    }

    const active = vscode.window.activeTextEditor?.document;
    const activeMarrowFile = active?.languageId === "marrow" ? active.uri.fsPath : undefined;

    if (config.project === undefined) {
      config.project = defaultProject(folder, activeMarrowFile);
    }
    if (config.stopOnEntry === undefined) {
      config.stopOnEntry = true;
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
