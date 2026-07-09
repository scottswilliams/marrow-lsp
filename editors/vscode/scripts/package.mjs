#!/usr/bin/env node
// Builds the release binaries, bundles them, and packages a single per-platform
// .vsix. The VS Code package target is host-detected by default but can be set
// explicitly with `--target <vsce-target>` so one runner can package for a
// cross-compiled triple (the Rust triple travels separately in CARGO_BUILD_TARGET,
// consumed by bundle.mjs). Reproducibility guard: packaging refuses to run while
// the upstream marrow crates are local `../marrow` path dependencies, because a
// published extension must resolve them at the pinned git revision. `--allow-path-deps`
// produces a deliberately non-reproducible dev/e2e VSIX and must never be published.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = resolve(here, "..");
const repoRoot = resolve(extensionDir, "..", "..");

const VSCODE_TARGETS = new Set([
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-x64",
]);

const HOST_TARGETS = {
  darwin: { arm64: "darwin-arm64", x64: "darwin-x64" },
  linux: { arm64: "linux-arm64", x64: "linux-x64" },
  win32: { x64: "win32-x64" },
};

function command(name) {
  return process.platform === "win32" ? `${name}.cmd` : name;
}

function parseArgs(argv) {
  let target;
  let allowPathDeps = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--target") {
      target = argv[i + 1];
      i += 1;
    } else if (arg.startsWith("--target=")) {
      target = arg.slice("--target=".length);
    } else if (arg === "--allow-path-deps") {
      allowPathDeps = true;
    } else {
      throw new Error(`Unknown argument to package.mjs: ${arg}`);
    }
  }
  return { target, allowPathDeps };
}

function resolveVscodeTarget(explicit) {
  if (explicit !== undefined) {
    if (!VSCODE_TARGETS.has(explicit)) {
      throw new Error(
        `Unknown VS Code package target: ${explicit}. Known targets: ${[...VSCODE_TARGETS].join(", ")}.`,
      );
    }
    return explicit;
  }
  const host = HOST_TARGETS[process.platform]?.[process.arch];
  if (host === undefined) {
    throw new Error(
      `Unsupported host VS Code package target for ${process.platform}/${process.arch}; pass --target explicitly.`,
    );
  }
  return host;
}

function cargoTargetDir() {
  const fromEnv = process.env.CARGO_TARGET_DIR?.trim();
  if (!fromEnv) {
    throw new Error("CARGO_TARGET_DIR must point at an explicit outside-repo build-output directory.");
  }

  const dir = resolve(fromEnv);
  const rel = relative(repoRoot, dir);
  if (rel === "" || (!rel.startsWith("..") && !isAbsolute(rel))) {
    throw new Error(`CARGO_TARGET_DIR must be outside the repository: ${dir}`);
  }

  return dir;
}

// Refuse to package a publishable .vsix while any upstream marrow crate resolves
// through a local `../marrow` path dependency: the resulting binary would depend on
// whatever happens to be checked out beside the repo, so the artifact is not
// reproducible from source. A release flips these to the pinned git revision first.
function assertReproducibleUpstream() {
  const manifest = readFileSync(join(repoRoot, "Cargo.toml"), "utf8");
  const offenders = [];
  for (const line of manifest.split("\n")) {
    const match = line.match(/^\s*(marrow-[a-z-]+)\s*=.*\bpath\s*=\s*"\.\.\/marrow/);
    if (match) {
      offenders.push(match[1]);
    }
  }
  if (offenders.length > 0) {
    throw new Error(
      `Refusing to package a .vsix: upstream marrow crates are still local path dependencies ` +
        `(${offenders.join(", ")}). A reproducible release must pin them to the git revision in ` +
        `pins/marrow-rev.toml (scripts/useGitUpstream.mjs). Pass --allow-path-deps only for a local ` +
        `dev or e2e VSIX that will not be published.`,
    );
  }
}

function run(name, args, env) {
  console.log(`$ ${name} ${args.join(" ")}`);
  execFileSync(command(name), args, {
    cwd: extensionDir,
    env,
    stdio: "inherit",
  });
}

const { target, allowPathDeps } = parseArgs(process.argv.slice(2));

if (allowPathDeps) {
  console.warn(
    "WARNING: packaging with --allow-path-deps; this VSIX resolves marrow through a local path " +
      "dependency and is not reproducible. It must not be published to a marketplace.",
  );
} else {
  assertReproducibleUpstream();
}

const vscodeTarget = resolveVscodeTarget(target);
const env = {
  ...process.env,
  CARGO_TARGET_DIR: cargoTargetDir(),
};

run("npm", ["run", "compile:release"], env);
run("npm", ["run", "bundle"], env);
run("npm", ["run", "package:vsix", "--", "--target", vscodeTarget], env);
