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

function makeClient() {
  const calls = [];
  const rootPath = [{ kind: "root", value: "Root" }];
  const bytesKeyPath = [
    ...rootPath,
    { kind: "key", value: { kind: "bytes", value: [10] } },
  ];
  const layerPath = [...rootPath, { kind: "layer", value: "history" }];

  return {
    calls,
    async sendRequest(method, params) {
      calls.push({ method, params });
      if (method === "marrow/savedRoots") {
        assert.equal(params, undefined);
        return { available: true, roots: ["Root"], store_snapshot: null };
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
            ],
            truncated: true,
            cursor: { kind: "int", value: 200 },
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
          };
        }
        throw new Error(`unexpected dataChildren path ${JSON.stringify(params)}`);
      }
      if (method === "marrow/dataRead") {
        if (
          deepEqual(params.segments, [
            ...bytesKeyPath,
            { kind: "field", value: "value" },
          ])
        ) {
          return {
            available: true,
            presence: "value_only",
            value: "42",
            store_snapshot: null,
          };
        }
        if (deepEqual(params.segments, layerPath)) {
          return {
            available: true,
            presence: "children_only",
            store_snapshot: null,
          };
        }
        throw new Error(`unexpected dataRead path ${JSON.stringify(params)}`);
      }
      throw new Error(`unexpected request ${method}`);
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

try {
  if (!existsSync(tsc)) {
    throw new Error("Run npm install in editors/vscode before this check.");
  }
  execFileSync(tsc, ["--project", join(extensionDir, "tsconfig.json"), "--outDir", outDir], {
    cwd: extensionDir,
    stdio: "inherit",
  });

  installVscodeStub();
  const { SavedResourceProvider } = await import(
    pathToFileURL(join(outDir, "savedResourceInspector.js"))
  );

  const provider = new SavedResourceProvider();
  const client = makeClient();
  provider.setClient(client);

  const roots = await provider.getChildren();
  assert.equal(roots.length, 1);

  assert.deepEqual(roots[0], { kind: "root", label: "Root" });
  const keys = await provider.getChildren(roots[0]);
  assert.equal(keys.length, 3);
  assert.equal(keys[0].label, "server-rendered-bytes-key");
  assert.deepEqual(keys[0].segments, [
    { kind: "root", value: "Root" },
    { kind: "key", value: { kind: "bytes", value: [10] } },
  ]);
  assert.deepEqual(keys[2], {
    kind: "more",
    label: "more children",
    segments: [{ kind: "root", value: "Root" }],
    cursor: { kind: "int", value: 200 },
  });

  const moreItem = provider.getTreeItem(keys[2]);
  assert.equal(moreItem.collapsibleState, 1, "cursor-backed continuation should expand");

  const nextKeys = await provider.getChildren(keys[2]);
  assert.equal(nextKeys.length, 2);
  assert.equal(nextKeys[0].label, "server-rendered-next-key");
  assert.deepEqual(client.calls.at(-1), {
    method: "marrow/dataChildren",
    params: {
      segments: [{ kind: "root", value: "Root" }],
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
    { kind: "root", value: "Root" },
    { kind: "layer", value: "history" },
  ];
  const layerChildren = await provider.getChildren(keys[1]);
  assert.deepEqual(layerChildren[0].segments, [
    ...layerPath,
    { kind: "field", value: "entry" },
  ]);
  assert.equal(
    hasReadFor(client.calls, layerPath),
    false,
    "layer expansion should ask for children without probing the value endpoint",
  );

  const fields = await provider.getChildren(keys[0]);
  assert.equal(fields.length, 1);
  assert.deepEqual(fields[0].segments, [
    { kind: "root", value: "Root" },
    { kind: "key", value: { kind: "bytes", value: [10] } },
    { kind: "field", value: "value" },
  ]);

  const values = await provider.getChildren(fields[0]);
  assert.deepEqual(values, [{ kind: "value", label: "42" }]);
  assert.equal(
    client.calls.some((call) => call.method === "marrow/dataRead"),
    true,
    "field expansion should read the leaf through the server",
  );
} finally {
  Module._load = restoreLoad;
  rmSync(outDir, { recursive: true, force: true });
}
