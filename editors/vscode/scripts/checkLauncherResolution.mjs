// Behavioral test for launcher binary resolution. It compiles src/launcher.ts with the
// esbuild already used for the extension bundle, then drives resolveBinaryPath with a
// fake host so every branch of the resolution order is asserted without a running editor:
// a configured hit, a configured miss (relative and non-file), the bundled fallback, the
// dev fallback gated by Production mode, and undefined when nothing is present.

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "esbuild";

const extensionDir = dirname(dirname(fileURLToPath(import.meta.url)));
const launcherSource = join(extensionDir, "src", "launcher.ts");

const outDir = mkdtempSync(join(tmpdir(), "marrow-launcher-test-"));
const outFile = join(outDir, "launcher.mjs");

try {
  await build({
    entryPoints: [launcherSource],
    outfile: outFile,
    bundle: true,
    format: "esm",
    platform: "node",
    logLevel: "silent",
  });

  const { resolveBinaryPath } = await import(pathToFileURL(outFile).href);

  const EXT = "/ext";
  const BUNDLED = join(EXT, "server", "marrow-lsp");
  const DEV = join(EXT, "..", "..", "target", "debug", "marrow-lsp");

  // A host whose files set names exactly which absolute paths exist.
  function host({ configured = {}, files = [], mode = "production" } = {}) {
    const present = new Set(files);
    return {
      configuredPath: (setting) => configured[setting],
      extensionPath: EXT,
      mode,
      isFile: (candidate) => present.has(candidate),
    };
  }

  const resolve = (h) => resolveBinaryPath(h, "server.path", "marrow-lsp");

  // Configured hit: an absolute configured file wins over everything.
  assert.equal(
    resolve(host({ configured: { "server.path": "/opt/marrow-lsp" }, files: ["/opt/marrow-lsp", BUNDLED] })),
    "/opt/marrow-lsp",
    "a configured absolute file is used",
  );

  // Configured miss (set but not a file): resolves to nothing, never falls back to bundled.
  assert.equal(
    resolve(host({ configured: { "server.path": "/opt/missing" }, files: [BUNDLED] })),
    undefined,
    "a configured-but-absent path does not silently fall back to the bundled binary",
  );

  // Configured miss (relative path): rejected as not absolute.
  assert.equal(
    resolve(host({ configured: { "server.path": "relative/marrow-lsp" }, files: ["relative/marrow-lsp"] })),
    undefined,
    "a relative configured path is rejected",
  );

  // Bundled hit: no configured value, the bundled binary exists.
  assert.equal(
    resolve(host({ files: [BUNDLED], mode: "production" })),
    BUNDLED,
    "the bundled binary is used when nothing is configured",
  );

  // Dev fallback in development mode when only the dev build exists.
  assert.equal(
    resolve(host({ files: [DEV], mode: "development" })),
    DEV,
    "the dev build is used outside a production install",
  );

  // Dev fallback refused in production mode even when the dev build exists.
  assert.equal(
    resolve(host({ files: [DEV], mode: "production" })),
    undefined,
    "the dev build is never used in a production install",
  );

  // Nothing present: undefined.
  assert.equal(
    resolve(host({ files: [], mode: "development" })),
    undefined,
    "resolution is undefined when no binary is found",
  );

  // A whitespace-only configured value is treated as unset and falls through to bundled.
  assert.equal(
    resolve(host({ configured: { "server.path": "   " }, files: [BUNDLED] })),
    BUNDLED,
    "a blank configured value is treated as unset",
  );

  console.log("launcher resolution: all branches asserted");
} finally {
  rmSync(outDir, { recursive: true, force: true });
}
