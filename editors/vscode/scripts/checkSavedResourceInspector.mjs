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
  return {
    calls,
    async sendRequest(method, params) {
      calls.push({ method, params });
      if (method === "marrow/savedRoots") {
        assert.equal(params, undefined);
        return { available: true, roots: ["Root"] };
      }
      if (method === "marrow/dataChildren") {
        if (params.segments.length === 1) {
          assert.deepEqual(params, {
            segments: [{ kind: "root", value: "Root" }],
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
            ],
            truncated: false,
            cursor: null,
          };
        }
        assert.deepEqual(params.segments, [
          { kind: "root", value: "Root" },
          { kind: "key", value: { kind: "bytes", value: [10] } },
        ]);
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
      if (method === "marrow/dataRead") {
        assert.deepEqual(params, {
          segments: [
            { kind: "root", value: "Root" },
            { kind: "key", value: { kind: "bytes", value: [10] } },
            { kind: "field", value: "value" },
          ],
        });
        return { available: true, presence: "value_only", value: "42" };
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
  assert.equal(keys.length, 1);
  assert.equal(keys[0].label, "server-rendered-bytes-key");
  assert.deepEqual(keys[0].segments, [
    { kind: "root", value: "Root" },
    { kind: "key", value: { kind: "bytes", value: [10] } },
  ]);

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
