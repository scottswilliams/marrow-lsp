// Behavioral test for the activation watchdog timing. Compiles src/watchdog.ts with
// esbuild and asserts the race contract that guards activation: a start that resolves
// before the deadline yields its value; a start that never resolves rejects once the
// deadline passes (so activation can surface an error and tear the client down rather
// than hanging on a wrong-arch or non-responding server binary).

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "esbuild";

const extensionDir = dirname(dirname(fileURLToPath(import.meta.url)));
const source = join(extensionDir, "src", "watchdog.ts");

const outDir = mkdtempSync(join(tmpdir(), "marrow-watchdog-test-"));
const outFile = join(outDir, "watchdog.mjs");

try {
  await build({
    entryPoints: [source],
    outfile: outFile,
    bundle: true,
    format: "esm",
    platform: "node",
    logLevel: "silent",
  });

  const { withStartTimeout } = await import(pathToFileURL(outFile).href);

  // A start that resolves before the deadline yields its value.
  const fast = await withStartTimeout(Promise.resolve("started"), 1000);
  assert.equal(fast, "started", "a start that completes in time resolves with its value");

  // A start that never resolves rejects once the short deadline passes.
  const never = new Promise(() => {});
  await assert.rejects(
    () => withStartTimeout(never, 20),
    /did not start within 20 ms/,
    "a start that never completes rejects at the deadline",
  );

  // A start that resolves just after a tiny delay, under a generous deadline, still wins.
  const slowish = new Promise((resolve) => setTimeout(() => resolve("ok"), 10));
  const value = await withStartTimeout(slowish, 1000);
  assert.equal(value, "ok", "a start that completes before the deadline is not spuriously timed out");

  console.log("activation watchdog: timing contract asserted");
} finally {
  rmSync(outDir, { recursive: true, force: true });
}
