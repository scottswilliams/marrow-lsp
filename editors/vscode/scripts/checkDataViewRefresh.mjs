// Behavioral test for the saved-data view refresh debounce. Compiles src/dataViewRefresh.ts with
// esbuild and asserts the coalescing and visibility contract: a burst of triggers folds into a
// single refresh, a hidden inspector skips the tree round-trip while still refreshing its watch
// targets, and an unchanged watch-target set is recognized so identical watchers are not recreated.

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "esbuild";

const extensionDir = dirname(dirname(fileURLToPath(import.meta.url)));
const source = join(extensionDir, "src", "dataViewRefresh.ts");

const outDir = mkdtempSync(join(tmpdir(), "marrow-data-view-refresh-test-"));
const outFile = join(outDir, "dataViewRefresh.mjs");

const DEBOUNCE_MS = 10;
const settle = () => new Promise((resolve) => setTimeout(resolve, DEBOUNCE_MS + 30));

function makeTargets(visible) {
  const counts = { watchTargets: 0, tree: 0 };
  return {
    counts,
    targets: {
      async refreshWatchTargets() {
        counts.watchTargets += 1;
      },
      refreshTree() {
        counts.tree += 1;
      },
      isInspectorVisible() {
        return visible;
      },
    },
  };
}

try {
  await build({
    entryPoints: [source],
    outfile: outFile,
    bundle: true,
    format: "esm",
    platform: "node",
    logLevel: "silent",
  });

  const { DataViewRefresher, sameWatchTargets } = await import(pathToFileURL(outFile).href);

  // A burst of rapid triggers coalesces into a single refresh per debounce window.
  {
    const { counts, targets } = makeTargets(true);
    const refresher = new DataViewRefresher(targets, DEBOUNCE_MS);
    for (let i = 0; i < 8; i += 1) {
      refresher.schedule();
    }
    await settle();
    assert.equal(counts.watchTargets, 1, "a burst of triggers coalesces to one watch-target refresh");
    assert.equal(counts.tree, 1, "a burst of triggers coalesces to one tree refresh");

    // A later trigger, after the window closed, refreshes again.
    refresher.schedule();
    await settle();
    assert.equal(counts.tree, 2, "a trigger in a new window refreshes again");
    refresher.dispose();
  }

  // A hidden inspector still refreshes its watch targets but skips the costly tree round-trip.
  {
    const { counts, targets } = makeTargets(false);
    const refresher = new DataViewRefresher(targets, DEBOUNCE_MS);
    refresher.schedule();
    refresher.schedule();
    await settle();
    assert.equal(counts.watchTargets, 1, "a hidden inspector still keeps its watch targets current");
    assert.equal(counts.tree, 0, "a hidden inspector skips the tree round-trip");
    refresher.dispose();
  }

  // A disposed refresher drops a pending refresh rather than firing after teardown.
  {
    const { counts, targets } = makeTargets(true);
    const refresher = new DataViewRefresher(targets, DEBOUNCE_MS);
    refresher.schedule();
    refresher.dispose();
    await settle();
    assert.equal(counts.tree, 0, "a disposed refresher drops its pending refresh");
  }

  // An unchanged watch-target set is recognized so identical watchers are not recreated.
  assert.equal(sameWatchTargets([], []), true, "two empty target sets are the same");
  assert.equal(
    sameWatchTargets(["/p/marrow.redb", "/p/marrow.lock"], ["/p/marrow.redb", "/p/marrow.lock"]),
    true,
    "identical target lists are the same set",
  );
  assert.equal(
    sameWatchTargets(["/p/marrow.redb"], ["/p/other.redb"]),
    false,
    "a different target path is a changed set",
  );
  assert.equal(
    sameWatchTargets(["/p/marrow.redb"], ["/p/marrow.redb", "/p/marrow.lock"]),
    false,
    "an added target is a changed set",
  );

  console.log("data-view refresh: debounce, visibility gate, and unchanged-set contract asserted");
} finally {
  rmSync(outDir, { recursive: true, force: true });
}
