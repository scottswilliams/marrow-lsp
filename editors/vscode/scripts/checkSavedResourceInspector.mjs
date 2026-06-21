import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import Module from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { tmpdir } from "node:os";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const extensionDir = dirname(scriptDir);
const outDir = mkdtempSync(join(tmpdir(), "marrow-vscode-saved-resource-"));
const tsc = join(extensionDir, "node_modules", ".bin", "tsc");
const restoreLoad = Module._load;
const baseSnapshot = {
  profile_version: "data.generation.v1",
  store_uid: "store_00000000000000000000000000000001",
  catalog_digest: "sha256:catalog",
  commit: {
    commit_id: 7,
    catalog_epoch: 3,
    source_digest: "sha256:source",
    layout_epoch: 2,
    engine_profile_digest: "77944eb86c08b665",
  },
  open_transaction: null,
  checked_source_digest: "sha256:checked",
};
const staleSnapshot = {
  ...baseSnapshot,
  checked_source_digest: "sha256:stale",
};
const unversionedSnapshot = {
  store_uid: baseSnapshot.store_uid,
  catalog_digest: baseSnapshot.catalog_digest,
  commit: baseSnapshot.commit,
  open_transaction: baseSnapshot.open_transaction,
  checked_source_digest: baseSnapshot.checked_source_digest,
};
const wrongProfileSnapshot = {
  ...baseSnapshot,
  profile_version: "data.generation.v0",
};

function installVscodeStub() {
  const watchers = [];

  class EventEmitter {
    constructor() {
      this.event = () => undefined;
    }

    fire() {
      return undefined;
    }
  }

  class TreeItem {
    constructor(label, collapsibleState) {
      this.label = label;
      this.collapsibleState = collapsibleState;
    }
  }

  class ThemeIcon {
    constructor(id) {
      this.id = id;
    }
  }

  class Uri {
    constructor(fsPath) {
      this.fsPath = fsPath;
    }

    toString() {
      return `file://${this.fsPath}`;
    }

    static file(fsPath) {
      return new Uri(fsPath);
    }
  }

  class RelativePattern {
    constructor(base, pattern) {
      this.base = base;
      this.pattern = pattern;
    }
  }

  function createFileSystemWatcher(pattern) {
    const watcher = {
      pattern,
      disposed: false,
      changeListeners: [],
      createListeners: [],
      deleteListeners: [],
      onDidChange(listener) {
        this.changeListeners.push(listener);
        return { dispose() {} };
      },
      onDidCreate(listener) {
        this.createListeners.push(listener);
        return { dispose() {} };
      },
      onDidDelete(listener) {
        this.deleteListeners.push(listener);
        return { dispose() {} };
      },
      dispose() {
        this.disposed = true;
      },
    };
    watchers.push(watcher);
    return watcher;
  }

  Module._load = function loadStubbedModule(request, parent, isMain) {
    if (request === "vscode") {
      return {
        EventEmitter,
        RelativePattern,
        ThemeIcon,
        TreeItem,
        TreeItemCollapsibleState: { None: 0, Collapsed: 1 },
        Uri,
        workspace: { createFileSystemWatcher },
      };
    }
    if (request === "vscode-languageclient/node") {
      return { FileChangeType: { Created: 1, Changed: 2, Deleted: 3 } };
    }
    return restoreLoad.call(this, request, parent, isMain);
  };

  return { watchers };
}

function fileUriFor(fsPath) {
  return `file://${fsPath}`;
}

function makeClient() {
  const calls = [];
  const rootPath = [{ kind: "root", value: "root-id" }];
  const staleRootPath = [{ kind: "root", value: "stale-root-id" }];
  const bytesKeyPath = [
    ...rootPath,
    { kind: "key", value: { kind: "bytes", value: [10] } },
  ];
  const layerPath = [...rootPath, { kind: "layer", value: "history" }];
  const staleValuePath = [...rootPath, { kind: "field", value: "stale" }];

  return {
    calls,
    async sendRequest(method, params) {
      calls.push({ method, params });
      if (method === "marrow/savedRoots") {
        assert.equal(params, undefined);
        return {
          available: true,
          roots: [
            { segment: rootPath[0], label: "Root" },
            { segment: staleRootPath[0], label: "StaleRoot" },
          ],
          store_snapshot: baseSnapshot,
        };
      }
      if (method === "marrow/dataChildren") {
        if (deepEqual(params.segments, rootPath) && params.cursor === null) {
          assert.deepEqual(params, {
            segments: rootPath,
            limit: 200,
            cursor: null,
          });
          return {
            available: true,
            children: [
              {
                segment: { kind: "key", value: { kind: "bytes", value: [10] } },
                label: "server-rendered-bytes-key",
              },
              {
                segment: { kind: "layer", value: "history" },
                label: "history",
              },
              {
                segment: { kind: "field", value: "stale" },
                label: "stale",
              },
            ],
            truncated: true,
            cursor: { kind: "int", value: 200 },
            store_snapshot: baseSnapshot,
          };
        }
        if (deepEqual(params.segments, staleRootPath) && params.cursor === null) {
          return {
            available: true,
            children: [
              {
                segment: { kind: "key", value: { kind: "string", value: "mixed" } },
                label: "mixed-generation-child",
              },
            ],
            truncated: false,
            cursor: null,
            store_snapshot: wrongProfileSnapshot,
          };
        }
        if (
          deepEqual(params.segments, rootPath) &&
          deepEqual(params.cursor, { kind: "int", value: 200 })
        ) {
          return {
            available: true,
            children: [
              {
                segment: { kind: "key", value: { kind: "string", value: "next" } },
                label: "server-rendered-next-key",
              },
            ],
            truncated: true,
            cursor: null,
            store_snapshot: baseSnapshot,
          };
        }
        if (deepEqual(params.segments, bytesKeyPath)) {
          return {
            available: true,
            children: [
              {
                segment: { kind: "field", value: "value" },
                label: "value",
              },
            ],
            truncated: false,
            cursor: null,
            store_snapshot: baseSnapshot,
          };
        }
        if (deepEqual(params.segments, layerPath)) {
          return {
            available: true,
            children: [
              {
                segment: { kind: "field", value: "entry" },
                label: "entry",
              },
            ],
            truncated: false,
            cursor: null,
            store_snapshot: baseSnapshot,
          };
        }
        throw new Error(`unexpected dataChildren path ${JSON.stringify(params)}`);
      }
      if (method === "marrow/dataRead") {
        if (deepEqual(params.segments, bytesKeyPath)) {
          assert.deepEqual(params, {
            segments: bytesKeyPath,
            preview_limit: 2048,
          });
          return {
            available: true,
            presence: "children_only",
            value_truncated: false,
            store_snapshot: baseSnapshot,
          };
        }
        if (
          deepEqual(params.segments, [
            ...bytesKeyPath,
            { kind: "field", value: "value" },
          ])
        ) {
          assert.deepEqual(params, {
            segments: [...bytesKeyPath, { kind: "field", value: "value" }],
            preview_limit: 2048,
          });
          return {
            available: true,
            presence: "value_only",
            value: "42",
            value_truncated: true,
            store_snapshot: baseSnapshot,
          };
        }
        if (deepEqual(params.segments, staleValuePath)) {
          return {
            available: true,
            presence: "value_only",
            value: "mixed-generation-value",
            value_truncated: false,
            store_snapshot: unversionedSnapshot,
          };
        }
        if (deepEqual(params.segments, layerPath)) {
          assert.deepEqual(params, {
            segments: layerPath,
            preview_limit: 2048,
          });
          return {
            available: true,
            presence: "children_only",
            value_truncated: false,
            store_snapshot: baseSnapshot,
          };
        }
        throw new Error(`unexpected dataRead path ${JSON.stringify(params)}`);
      }
      throw new Error(`unexpected request ${method}`);
    },
  };
}

function makeRootsClient(storeSnapshot) {
  return {
    async sendRequest(method, params) {
      if (method !== "marrow/savedRoots") {
        throw new Error(`unexpected request ${method}`);
      }
      assert.equal(params, undefined);
      return {
        available: true,
        roots: [{ segment: { kind: "root", value: "root-id" }, label: "Root" }],
        store_snapshot: storeSnapshot,
      };
    },
  };
}

function deepEqual(left, right) {
  try {
    assert.deepEqual(left, right);
    return true;
  } catch {
    return false;
  }
}

function hasReadFor(calls, segments) {
  return calls.some(
    (call) =>
      call.method === "marrow/dataRead" &&
      call.params !== undefined &&
      deepEqual(call.params.segments, segments),
  );
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function activeWatcherPaths(watchers) {
  return watchers
    .filter((watcher) => !watcher.disposed)
    .map((watcher) => join(watcher.pattern.base, watcher.pattern.pattern));
}

try {
  if (!existsSync(tsc)) {
    throw new Error("Run npm install in editors/vscode before this check.");
  }
  execFileSync(tsc, ["--project", join(extensionDir, "tsconfig.json"), "--outDir", outDir], {
    cwd: extensionDir,
    stdio: "inherit",
  });

  const vscodeStub = installVscodeStub();
  const { SavedResourceProvider } = await import(
    pathToFileURL(join(outDir, "savedResourceInspector.js"))
  );
  const { SavedDataWatchTargetRegistry } = await import(
    pathToFileURL(join(outDir, "extension.js"))
  );

  for (const storeSnapshot of [unversionedSnapshot, wrongProfileSnapshot, null]) {
    const malformedRootsProvider = new SavedResourceProvider();
    malformedRootsProvider.setClient(makeRootsClient(storeSnapshot));
    assert.deepEqual(await malformedRootsProvider.getChildren(), [
      { kind: "unavailable", label: "store unavailable" },
    ]);
  }

  const provider = new SavedResourceProvider();
  const client = makeClient();
  provider.setClient(client);

  const watchTarget = join(outDir, "project", ".marrow", "data", "marrow.redb");
  const watchClientCalls = [];
  const watchEventLog = [];
  const notificationRequests = [];
  let refreshCount = 0;
  const watchProvider = new SavedResourceProvider();
  watchProvider.refresh = () => {
    refreshCount += 1;
    watchEventLog.push({ kind: "refresh" });
  };
  const watchRegistry = new SavedDataWatchTargetRegistry(
    {
      async sendRequest(method, params) {
        watchClientCalls.push({ method, params });
        assert.equal(method, "marrow/dataWatchTargets");
        assert.equal(params, undefined);
        return { paths: [watchTarget] };
      },
      sendNotification(method, params) {
        watchEventLog.push({ kind: "notify", method, params });
        const request = deferred();
        notificationRequests.push(request);
        return request.promise;
      },
    },
    watchProvider,
  );
  await watchRegistry.refresh();
  assert.deepEqual(watchClientCalls, [
    { method: "marrow/dataWatchTargets", params: undefined },
  ]);
  assert.equal(vscodeStub.watchers.length, 1);
  assert.equal(
    vscodeStub.watchers[0].pattern.base,
    dirname(watchTarget),
    "watch base should come from the LSP-supplied target path",
  );
  assert.equal(
    vscodeStub.watchers[0].pattern.pattern,
    "marrow.redb",
    "watch pattern should be the LSP-supplied target filename",
  );
  const watchNotification = (type) => ({
    kind: "notify",
    method: "workspace/didChangeWatchedFiles",
    params: { changes: [{ uri: fileUriFor(watchTarget), type }] },
  });

  vscodeStub.watchers[0].changeListeners[0]();
  assert.equal(refreshCount, 0, "change refresh must wait for notification settlement");
  assert.deepEqual(watchEventLog, [watchNotification(2)]);
  notificationRequests[0].resolve();
  await Promise.resolve();
  assert.equal(refreshCount, 1);
  assert.deepEqual(watchEventLog, [watchNotification(2), { kind: "refresh" }]);

  vscodeStub.watchers[0].createListeners[0]();
  assert.equal(refreshCount, 1, "create refresh must wait for notification settlement");
  assert.deepEqual(watchEventLog, [
    watchNotification(2),
    { kind: "refresh" },
    watchNotification(1),
  ]);
  notificationRequests[1].reject(new Error("notification failed"));
  await Promise.resolve();
  assert.equal(refreshCount, 2, "create refresh must still fire after notification failure");
  assert.deepEqual(watchEventLog, [
    watchNotification(2),
    { kind: "refresh" },
    watchNotification(1),
    { kind: "refresh" },
  ]);

  vscodeStub.watchers[0].deleteListeners[0]();
  assert.equal(refreshCount, 2, "delete refresh must wait for notification settlement");
  assert.deepEqual(watchEventLog, [
    watchNotification(2),
    { kind: "refresh" },
    watchNotification(1),
    { kind: "refresh" },
    watchNotification(3),
  ]);
  notificationRequests[2].resolve();
  await Promise.resolve();
  assert.equal(refreshCount, 3);
  assert.deepEqual(watchEventLog, [
    watchNotification(2),
    { kind: "refresh" },
    watchNotification(1),
    { kind: "refresh" },
    watchNotification(3),
    { kind: "refresh" },
  ]);
  watchRegistry.dispose();
  assert.equal(vscodeStub.watchers[0].disposed, true);

  const ignoredTargetRegistry = new SavedDataWatchTargetRegistry(
    {
      async sendRequest(method, params) {
        assert.equal(method, "marrow/dataWatchTargets");
        assert.equal(params, undefined);
        return { paths: ["relative/marrow.redb", fileUriFor(watchTarget)] };
      },
      sendNotification() {
        throw new Error("ignored watch targets must not notify the language server");
      },
    },
    watchProvider,
  );
  await ignoredTargetRegistry.refresh();
  assert.deepEqual(
    activeWatcherPaths(vscodeStub.watchers),
    [],
    "relative paths and file URLs are not absolute filesystem watch targets",
  );
  ignoredTargetRegistry.dispose();

  const racingRequests = [];
  const racingRegistry = new SavedDataWatchTargetRegistry(
    {
      sendRequest(method, params) {
        assert.equal(method, "marrow/dataWatchTargets");
        assert.equal(params, undefined);
        const request = deferred();
        racingRequests.push(request);
        return request.promise;
      },
    },
    watchProvider,
  );
  const watchTargetA = join(outDir, "project", "data", "a.redb");
  const watchTargetB = join(outDir, "project", "data", "b.redb");
  const watchTargetC = join(outDir, "project", "data", "c.redb");

  const refreshA = racingRegistry.refresh();
  assert.equal(racingRequests.length, 1);
  const refreshB = racingRegistry.refresh();
  assert.equal(racingRequests.length, 2);
  racingRequests[1].resolve({ paths: [watchTargetB] });
  await refreshB;
  assert.deepEqual(activeWatcherPaths(vscodeStub.watchers), [watchTargetB]);

  racingRequests[0].resolve({ paths: [watchTargetA] });
  await refreshA;
  assert.deepEqual(
    activeWatcherPaths(vscodeStub.watchers),
    [watchTargetB],
    "a stale watch-target response must not replace or duplicate the current watchers",
  );

  const refreshC = racingRegistry.refresh();
  assert.equal(racingRequests.length, 3);
  racingRegistry.dispose();
  const watchersBeforeLateDisposeResponse = vscodeStub.watchers.length;
  racingRequests[2].resolve({ paths: [watchTargetC] });
  await refreshC;
  assert.deepEqual(activeWatcherPaths(vscodeStub.watchers), []);
  assert.equal(
    vscodeStub.watchers.length,
    watchersBeforeLateDisposeResponse,
    "a disposed registry must not create watchers from a late response",
  );

  const roots = await provider.getChildren();
  assert.equal(roots.length, 2);

  assert.deepEqual(roots[0], {
    kind: "root",
    label: "Root",
    segment: { kind: "root", value: "root-id" },
    snapshot: baseSnapshot,
  });
  assert.equal(roots[0].snapshot.profile_version, "data.generation.v1");
  assert.deepEqual(roots[1], {
    kind: "root",
    label: "StaleRoot",
    segment: { kind: "root", value: "stale-root-id" },
    snapshot: baseSnapshot,
  });
  const keys = await provider.getChildren(roots[0]);
  assert.equal(keys.length, 4);
  assert.equal(keys[0].label, "server-rendered-bytes-key");
  assert.deepEqual(keys[0].segments, [
    { kind: "root", value: "root-id" },
    { kind: "key", value: { kind: "bytes", value: [10] } },
  ]);
  assert.deepEqual(keys[0].snapshot, baseSnapshot);
  assert.deepEqual(keys[1].snapshot, baseSnapshot);
  assert.deepEqual(keys[2].snapshot, baseSnapshot);
  assert.deepEqual(keys[3], {
    kind: "more",
    label: "more children",
    segments: [{ kind: "root", value: "root-id" }],
    cursor: { kind: "int", value: 200 },
    snapshot: baseSnapshot,
  });

  const moreItem = provider.getTreeItem(keys[3]);
  assert.equal(moreItem.collapsibleState, 1, "cursor-backed continuation should expand");

  const nextKeys = await provider.getChildren(keys[3]);
  assert.equal(nextKeys.length, 2);
  assert.equal(nextKeys[0].label, "server-rendered-next-key");
  assert.deepEqual(nextKeys[0].snapshot, baseSnapshot);
  assert.deepEqual(client.calls.at(-1), {
    method: "marrow/dataChildren",
    params: {
      segments: [{ kind: "root", value: "root-id" }],
      limit: 200,
      cursor: { kind: "int", value: 200 },
    },
  });
  assert.deepEqual(nextKeys[1], {
    kind: "truncated",
    label: "additional children unavailable",
  });
  const terminalItem = provider.getTreeItem(nextKeys[1]);
  assert.equal(terminalItem.collapsibleState, 0, "cursor-less truncation should not expand");

  const layerPath = [
    { kind: "root", value: "root-id" },
    { kind: "layer", value: "history" },
  ];
  const beforeLayerCalls = client.calls.length;
  const layerChildren = await provider.getChildren(keys[1]);
  assert.deepEqual(
    client.calls.slice(beforeLayerCalls).map((call) => call.method),
    ["marrow/dataRead", "marrow/dataChildren"],
    "layer expansion should probe presence before asking for children",
  );
  assert.deepEqual(layerChildren[0].segments, [
    ...layerPath,
    { kind: "field", value: "entry" },
  ]);
  assert.equal(
    hasReadFor(client.calls, layerPath),
    true,
    "layer expansion should read presence through the server",
  );

  const beforeKeyCalls = client.calls.length;
  const fields = await provider.getChildren(keys[0]);
  assert.deepEqual(
    client.calls.slice(beforeKeyCalls).map((call) => call.method),
    ["marrow/dataRead", "marrow/dataChildren"],
    "segment expansion should probe presence before asking for children",
  );
  assert.equal(fields.length, 1);
  assert.deepEqual(fields[0].segments, [
    { kind: "root", value: "root-id" },
    { kind: "key", value: { kind: "bytes", value: [10] } },
    { kind: "field", value: "value" },
  ]);
  assert.deepEqual(fields[0].snapshot, baseSnapshot);

  const values = await provider.getChildren(fields[0]);
  assert.deepEqual(values, [{ kind: "value", label: "42", valueTruncated: true }]);
  const valueItem = provider.getTreeItem(values[0]);
  assert.equal(valueItem.description, "truncated");
  assert.equal(
    client.calls.some((call) => call.method === "marrow/dataRead"),
    true,
    "value expansion should read presence through the server",
  );

  const staleRootChildren = await provider.getChildren(roots[1]);
  assert.deepEqual(staleRootChildren, [{ kind: "changed", label: "data changed; refresh" }]);

  const staleValue = await provider.getChildren(keys[2]);
  assert.deepEqual(staleValue, [{ kind: "changed", label: "data changed; refresh" }]);
} finally {
  Module._load = restoreLoad;
  rmSync(outDir, { recursive: true, force: true });
}
