#!/usr/bin/env node
// Builds the two release transport binaries (marrow-lsp, marrow-dap) and copies
// them into editors/vscode/server/ with the exec bit set, so `vsce package` can
// bundle a self-contained extension that runs without a local Rust toolchain.
//
// The Rust target directory is external by default (the repo's build harness sets
// CARGO_TARGET_DIR); this script honors that and falls back to the in-tree
// workspace `target/` when it is unset. Cross-compilation is supported by passing
// a Rust target triple via CARGO_BUILD_TARGET (the CI workflow uses this); the
// binaries are then found under target/<triple>/release/.

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = resolve(here, "..");
// editors/vscode -> editors -> repo root.
const repoRoot = resolve(extensionDir, "..", "..");

const EXE = process.platform === "win32" ? ".exe" : "";
const BINARIES = ["marrow-lsp", "marrow-dap"];
// An optional Rust target triple (e.g. aarch64-apple-darwin). When set, cargo
// emits to target/<triple>/release/ and we read the binaries from there.
const rustTarget = process.env.CARGO_BUILD_TARGET?.trim();

function run(command, args) {
  console.log(`$ ${command} ${args.join(" ")}`);
  execFileSync(command, args, { cwd: repoRoot, stdio: "inherit" });
}

// Resolve the cargo target directory the same way cargo does: $CARGO_TARGET_DIR
// when set, else the workspace `target/`.
function targetDir() {
  const fromEnv = process.env.CARGO_TARGET_DIR?.trim();
  return fromEnv ? resolve(fromEnv) : join(repoRoot, "target");
}

// The release output directory, accounting for an optional cross-compile triple.
function releaseDir() {
  const base = targetDir();
  return rustTarget ? join(base, rustTarget, "release") : join(base, "release");
}

function build() {
  const args = ["build", "--release", "-p", "marrow-lsp", "-p", "marrow-dap"];
  if (rustTarget) {
    args.push("--target", rustTarget);
  }
  run("cargo", args);
}

function bundle() {
  const from = releaseDir();
  const serverDir = join(extensionDir, "server");
  mkdirSync(serverDir, { recursive: true });

  for (const name of BINARIES) {
    const src = join(from, `${name}${EXE}`);
    if (!existsSync(src)) {
      throw new Error(
        `release binary not found: ${src}\n` +
          "Run the release build first (this script does that), or check CARGO_TARGET_DIR / CARGO_BUILD_TARGET.",
      );
    }
    const dest = join(serverDir, `${name}${EXE}`);
    copyFileSync(src, dest);
    // The extension launches these directly; make them executable on unix.
    chmodSync(dest, 0o755);
    console.log(`bundled ${dest}`);
  }
}

build();
bundle();
console.log("bundle complete");
