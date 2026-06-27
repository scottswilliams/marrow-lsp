import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import Module from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const extensionDir = dirname(scriptDir);
const outDir = mkdtempSync(join(tmpdir(), "marrow-vscode-data-integrity-"));
const tsc = join(extensionDir, "node_modules", ".bin", "tsc");
const restoreLoad = Module._load;

const baseSnapshot = {
  profile_version: "data.generation.v1",
  store_uid: "store_00000000000000000000000000000001",
  catalog_digest: "sha256:store-catalog",
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

const baseBoundary = {
  sourceAnalysisGeneration: {
    profileVersion: "analysis.generation.v1",
    sourceIdentity: "sha256:source-identity",
    configDigest: "sha256:config",
    checkedSourceDigest: "sha256:checked",
    readOnlyContextDigest: "sha256:readonly",
    acceptedCatalog: { epoch: 3, digest: "sha256:accepted-catalog" },
    proposalCatalog: null,
  },
  storeSnapshot: baseSnapshot,
  compatibility: { verdict: "admitted" },
  watchTargets: [
    { kind: "store_file", path: "/project/.marrow/data/marrow.redb" },
    { kind: "catalog_lock", path: "/project/marrow.lock" },
  ],
};

const malformedBoundary = {
  ...baseBoundary,
  compatibility: null,
};

const staleBoundary = {
  ...baseBoundary,
  storeSnapshot: {
    ...baseSnapshot,
    checked_source_digest: "sha256:stale",
  },
};

const negativeBoundary = {
  ...baseBoundary,
  sourceAnalysisGeneration: {
    ...baseBoundary.sourceAnalysisGeneration,
    acceptedCatalog: { epoch: -1, digest: "sha256:accepted-catalog" },
  },
  storeSnapshot: {
    ...baseSnapshot,
    commit: {
      ...baseSnapshot.commit,
      commit_id: -1,
    },
    open_transaction: { depth: -1 },
  },
};

function installVscodeStub() {
  const messages = [];
  const channels = [];

  class OutputChannel {
    constructor(name) {
      this.name = name;
      this.lines = [];
      this.shown = false;
      this.disposed = false;
    }

    clear() {
      this.lines = [];
    }

    appendLine(line) {
      this.lines.push(line);
    }

    show() {
      this.shown = true;
    }

    dispose() {
      this.disposed = true;
    }
  }

  Module._load = function loadStubbedModule(request, parent, isMain) {
    if (request === "vscode") {
      return {
        window: {
          createOutputChannel(name) {
            const channel = new OutputChannel(name);
            channels.push(channel);
            return channel;
          },
          showInformationMessage(message) {
            messages.push({ kind: "info", message });
            return Promise.resolve(undefined);
          },
        },
      };
    }
    if (request === "vscode-languageclient/node") {
      return {};
    }
    return restoreLoad.call(this, request, parent, isMain);
  };

  return { channels, messages };
}

function makeClient(responseOrResponses) {
  const responses = Array.isArray(responseOrResponses)
    ? responseOrResponses
    : [responseOrResponses];
  const calls = [];
  return {
    calls,
    async sendRequest(method, params) {
      calls.push({ method, params });
      assert.equal(method, "marrow/dataIntegrity");
      assert.equal(params, undefined);
      return responses[Math.min(calls.length - 1, responses.length - 1)];
    },
  };
}

const finding = {
  code: "store.missing",
  kind: "missing_member",
  message: "missing member",
  source_span: { path: "^Root" },
  help: "run migration",
};

try {
  if (!existsSync(tsc)) {
    throw new Error("Run npm install in editors/vscode before this check.");
  }
  execFileSync(tsc, ["--project", join(extensionDir, "tsconfig.json"), "--outDir", outDir], {
    cwd: extensionDir,
    stdio: "inherit",
  });

  const vscodeStub = installVscodeStub();
  const { MarrowDataIntegrity } = await import(
    pathToFileURL(join(outDir, "dataIntegrity.js"))
  );

  const unavailableResult = {
    available: true,
    findings: [],
    scanned: 0,
    truncated: false,
    store_snapshot: baseSnapshot,
    data_view_boundary: null,
  };
  const validResult = {
    available: true,
    findings: [finding],
    scanned: 2,
    truncated: true,
    store_snapshot: baseSnapshot,
    data_view_boundary: baseBoundary,
  };
  const client = makeClient([validResult, unavailableResult]);
  const integrity = new MarrowDataIntegrity(client);
  await integrity.run();

  assert.deepEqual(client.calls, [{ method: "marrow/dataIntegrity", params: undefined }]);
  assert.equal(vscodeStub.channels.length, 1);
  assert.deepEqual(vscodeStub.channels[0].lines, [
    "Data view: admitted source sha256:source-identity; checked sha256:checked; accepted catalog 3 sha256:accepted-catalog; store commit 7; watch targets 2",
    "Scanned 2 location(s) (truncated; scan limit reached)",
    "[store.missing] ^Root: missing member Help: run migration",
  ]);
  assert.equal(vscodeStub.channels[0].shown, true);
  assert.equal(vscodeStub.messages.at(-1).message, "1 advisory issue(s) found");

  await integrity.run();
  assert.deepEqual(vscodeStub.channels[0].lines, ["Data view unavailable"]);
  assert.match(
    vscodeStub.messages.at(-1).message,
    /needs opt-in live data/,
    "unavailable reports should not leave the previous report visible",
  );

  for (const dataViewBoundary of [null, malformedBoundary]) {
    const beforeChannels = vscodeStub.channels.length;
    const unavailable = new MarrowDataIntegrity(
      makeClient({
        available: true,
        findings: [],
        scanned: 0,
        truncated: false,
        store_snapshot: baseSnapshot,
        data_view_boundary: dataViewBoundary,
      }),
    );
    await unavailable.run();
    assert.equal(
      vscodeStub.channels.length,
      beforeChannels,
      "reports without a valid data-view boundary must not create an output channel",
    );
    assert.match(
      vscodeStub.messages.at(-1).message,
      /needs opt-in live data/,
      "reports without a valid data-view boundary should use the unavailable contract",
    );
  }

  for (const [storeSnapshot, dataViewBoundary] of [
    [baseSnapshot, staleBoundary],
    [baseSnapshot, negativeBoundary],
    [{ ...baseSnapshot, commit: undefined }, baseBoundary],
  ]) {
    const beforeChannels = vscodeStub.channels.length;
    const unavailable = new MarrowDataIntegrity(
      makeClient({
        available: true,
        findings: [],
        scanned: 0,
        truncated: false,
        store_snapshot: storeSnapshot,
        data_view_boundary: dataViewBoundary,
      }),
    );
    await unavailable.run();
    assert.equal(
      vscodeStub.channels.length,
      beforeChannels,
      "mismatched and unsigned-invalid boundaries must not create an output channel",
    );
    assert.match(
      vscodeStub.messages.at(-1).message,
      /needs opt-in live data/,
      "mismatched and unsigned-invalid boundaries should use the unavailable contract",
    );
  }

  integrity.dispose();
  assert.equal(vscodeStub.channels[0].disposed, true);
} finally {
  Module._load = restoreLoad;
  rmSync(outDir, { recursive: true, force: true });
}
