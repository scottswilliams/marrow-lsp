// Packaged-extension end-to-end driver. Downloads a real VS Code, installs the built
// host VSIX into it, opens the shelf fixture project, and runs the in-editor suite that
// drives one live request per editor transport against the real bundled binaries (LSP
// hover, DAP launch/stop-on-entry). The MCP transport is not bundled in the VSIX, so it
// is exercised separately by mcpTransport.mjs.
//
// This needs a display; CI runs it under xvfb. It requires @vscode/test-electron and
// mocha, installed at job time (kept out of package.json so `npm ci`/audit stay lean).

import { readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runTests } from "@vscode/test-electron";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = resolve(here, "..", "..");
const repoRoot = resolve(extensionDir, "..", "..");
const fixture = join(repoRoot, "fixtures", "shelf");

// The VSIX must be built first (npm run package -- --allow-path-deps produces a dev
// VSIX with the real bundled binaries). Installing it makes the test drive the packaged
// activation events and bundled binaries, so a broken activation event or a wrong-arch
// binary fails here instead of shipping dead.
function findVsix() {
  const explicit = process.env.MARROW_VSIX?.trim();
  if (explicit) {
    return explicit;
  }
  const found = readdirSync(extensionDir).filter((name) => name.endsWith(".vsix"));
  if (found.length !== 1) {
    throw new Error(`expected exactly one built .vsix in ${extensionDir}, found ${found.length}`);
  }
  return join(extensionDir, found[0]);
}

async function main() {
  await runTests({
    extensionDevelopmentPath: extensionDir,
    extensionTestsPath: join(here, "suite", "index.cjs"),
    launchArgs: [
      fixture,
      "--install-extension",
      findVsix(),
      "--disable-workspace-trust",
      "--disable-extensions=false",
    ],
  });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
