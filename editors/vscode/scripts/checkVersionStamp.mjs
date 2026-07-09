// Version-stamp gate. A release ships one version, so the extension manifest, the
// Cargo workspace, the top CHANGELOG entry, and the git tag being released must all
// name the same version. Drift between any pair means a mislabelled artifact, so this
// runs fail-closed as the first release preflight. The git-tag comparison is enabled
// only when a tag is supplied (env RELEASE_TAG or --tag), so it also runs cleanly on
// PRs where the manifest/Cargo/CHANGELOG triple is still worth checking for drift.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const extensionDir = dirname(dirname(fileURLToPath(import.meta.url)));
const repoRoot = dirname(dirname(extensionDir));

function packageVersion() {
  const manifest = JSON.parse(readFileSync(join(extensionDir, "package.json"), "utf8"));
  if (typeof manifest.version !== "string") {
    throw new Error("editors/vscode/package.json has no string version");
  }
  return manifest.version;
}

// Read version from [workspace.package] specifically, not the first `version =` in the
// file (workspace.dependencies also carries version pins).
function cargoWorkspaceVersion() {
  const toml = readFileSync(join(repoRoot, "Cargo.toml"), "utf8");
  const lines = toml.split("\n");
  let inWorkspacePackage = false;
  for (const raw of lines) {
    const line = raw.trim();
    if (line.startsWith("[")) {
      inWorkspacePackage = line === "[workspace.package]";
      continue;
    }
    if (inWorkspacePackage) {
      const match = line.match(/^version\s*=\s*"([^"]+)"/);
      if (match) {
        return match[1];
      }
    }
  }
  throw new Error("could not find version under [workspace.package] in Cargo.toml");
}

// The top-most `## <version>` heading in the changelog.
function changelogVersion() {
  const changelog = readFileSync(join(extensionDir, "CHANGELOG.md"), "utf8");
  for (const line of changelog.split("\n")) {
    const match = line.match(/^##\s+(\S+)/);
    if (match) {
      return match[1];
    }
  }
  throw new Error("no `## <version>` heading found in CHANGELOG.md");
}

function tagVersion() {
  const argTag = process.argv.slice(2).find((arg) => arg.startsWith("--tag="));
  const raw = argTag ? argTag.slice("--tag=".length) : process.env.RELEASE_TAG;
  if (!raw) {
    return undefined;
  }
  // Accept a bare version or a refs/tags/vX.Y.Z / vX.Y.Z ref.
  const stripped = raw.replace(/^refs\/tags\//, "").replace(/^v/, "");
  return stripped;
}

const stamps = {
  "package.json": packageVersion(),
  "Cargo [workspace.package]": cargoWorkspaceVersion(),
  "CHANGELOG.md": changelogVersion(),
};

const tag = tagVersion();
if (tag !== undefined) {
  stamps["git tag"] = tag;
}

const values = new Set(Object.values(stamps));
if (values.size !== 1) {
  const detail = Object.entries(stamps)
    .map(([name, value]) => `  ${name}: ${value}`)
    .join("\n");
  throw new Error(`version stamps disagree:\n${detail}`);
}

const compared = Object.keys(stamps).join(", ");
console.log(`version stamps agree at ${[...values][0]} (${compared})`);
