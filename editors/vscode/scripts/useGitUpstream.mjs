// Release-time upstream pinning. Local development resolves the marrow crates as
// `../marrow` path dependencies; a reproducible release resolves them at the single
// git revision recorded in pins/marrow-rev.toml. This rewrites the `[workspace.dependencies]`
// marrow-* path entries to `git + rev` in place so `package.mjs` can build a reproducible,
// publishable .vsix (the reproducibility guard then passes). It runs on the release runner
// against a throwaway checkout; it is not committed state.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const extensionDir = dirname(dirname(fileURLToPath(import.meta.url)));
const repoRoot = dirname(dirname(extensionDir));

const gitUrl = process.env.MARROW_GIT_URL?.trim() || "https://github.com/scottswilliams/marrow";

function pinnedRev() {
  const pin = readFileSync(join(repoRoot, "pins", "marrow-rev.toml"), "utf8");
  const match = pin.match(/^rev\s*=\s*"([0-9a-f]{40})"/m);
  if (!match) {
    throw new Error("pins/marrow-rev.toml does not contain a 40-hex rev");
  }
  return match[1];
}

const rev = pinnedRev();
const cargoPath = join(repoRoot, "Cargo.toml");
const original = readFileSync(cargoPath, "utf8");

let rewrote = 0;
const rewritten = original
  .split("\n")
  .map((line) => {
    if (!/^\s*marrow-[a-z-]+\s*=/.test(line)) {
      return line;
    }
    const next = line.replace(
      /\bpath\s*=\s*"\.\.\/marrow[^"]*"/,
      `git = "${gitUrl}", rev = "${rev}"`,
    );
    if (next !== line) {
      rewrote += 1;
    }
    return next;
  })
  .join("\n");

if (rewrote === 0) {
  throw new Error("no ../marrow path dependencies found to pin; refusing a silent no-op release rewrite");
}

writeFileSync(cargoPath, rewritten);
console.log(`pinned ${rewrote} upstream marrow dependencies to ${gitUrl}@${rev}`);
