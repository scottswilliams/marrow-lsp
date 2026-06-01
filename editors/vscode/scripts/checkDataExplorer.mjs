import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import Module from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { tmpdir } from "node:os";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const extensionDir = dirname(scriptDir);
const outDir = mkdtempSync(join(tmpdir(), "marrow-vscode-data-explorer-"));
const tsc = join(extensionDir, "node_modules", ".bin", "tsc");
const restoreLoad = Module._load;

function installVscodeStub() {
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

  Module._load = function loadStubbedModule(request, parent, isMain) {
    if (request === "vscode") {
      return {
        EventEmitter,
        ThemeIcon,
        TreeItem,
        TreeItemCollapsibleState: { None: 0, Collapsed: 1 },
      };
    }
    if (request === "vscode-languageclient/node") {
      return {};
    }
    return restoreLoad.call(this, request, parent, isMain);
  };
}

function makeClient(children) {
  return {
    async sendRequest(method, params) {
      if (method === "marrow/savedRoots") {
        return { available: true, roots: ["Root"] };
      }
      if (method === "marrow/savedChildren") {
        assert.deepEqual(params, { path: [{ root: "Root" }] });
        return { available: true, children, more: false };
      }
      throw new Error(`unexpected request ${method}`);
    },
  };
}

try {
  if (!existsSync(tsc)) {
    throw new Error("Run npm install in editors/vscode before this check.");
  }
  execFileSync(tsc, ["--project", join(extensionDir, "tsconfig.json"), "--outDir", outDir], {
    cwd: extensionDir,
    stdio: "inherit",
  });

  installVscodeStub();
  const { MarrowDataProvider } = await import(pathToFileURL(join(outDir, "dataExplorer.js")));

  const provider = new MarrowDataProvider();
  provider.setClient(
    makeClient([
      {
        kind: "key",
        key: { str: "canonical" },
        appendSegment: { index_key: { str: "canonical" } },
      },
      { kind: "key", key: { str: "legacy-key" } },
      { kind: "name", name: "legacy_field" },
    ]),
  );

  const roots = await provider.getChildren();
  assert.equal(roots.length, 1);
  const children = await provider.getChildren(roots[0]);

  assert.deepEqual(children[0], {
    kind: "store",
    path: [{ root: "Root" }, { index_key: { str: "canonical" } }],
    label: "\"canonical\"",
    role: undefined,
    detail: undefined,
    type: undefined,
  });
  assert.deepEqual(children.slice(1), [
    { kind: "unavailable", label: "path metadata unavailable" },
    { kind: "unavailable", label: "path metadata unavailable" },
  ]);
} finally {
  Module._load = restoreLoad;
  rmSync(outDir, { recursive: true, force: true });
}
