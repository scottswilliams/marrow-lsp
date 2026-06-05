#!/usr/bin/env node
// Builds the two release transport binaries (marrow-lsp, marrow-dap) and copies
// them into editors/vscode/server/ with the exec bit set, so `vsce package` can
// bundle a self-contained extension that runs without a local Rust toolchain.
//
// CARGO_TARGET_DIR is required so package smoke uses an explicit build-output
// directory outside the repository. Cross-compilation is supported by passing a
// Rust target triple via CARGO_BUILD_TARGET.

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = resolve(here, "..");
// editors/vscode -> editors -> repo root.
const repoRoot = resolve(extensionDir, "..", "..");
const manifestPath = join(repoRoot, "Cargo.toml");

const EXE = process.platform === "win32" ? ".exe" : "";
const BINARIES = ["marrow-lsp", "marrow-dap"];
// An optional Rust target triple (e.g. aarch64-apple-darwin). When set, cargo
// emits to target/<triple>/release/ and we read the binaries from there.
const rustTarget = process.env.CARGO_BUILD_TARGET?.trim();

function run(command, args, env = process.env) {
  console.log(`$ ${command} ${args.join(" ")}`);
  execFileSync(command, args, { cwd: repoRoot, env, stdio: "inherit" });
}

function targetDir() {
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

// The release output directory, accounting for an optional cross-compile triple.
function releaseDir(cargoTargetDir) {
  const base = cargoTargetDir;
  return rustTarget ? join(base, rustTarget, "release") : join(base, "release");
}

function build(cargoTargetDir) {
  const args = [
    "build",
    "--manifest-path",
    manifestPath,
    "--release",
    "-p",
    "marrow-lsp",
    "-p",
    "marrow-dap",
  ];
  if (rustTarget) {
    args.push("--target", rustTarget);
  }
  run("cargo", args, { ...process.env, CARGO_TARGET_DIR: cargoTargetDir });
}

function bundle(cargoTargetDir) {
  const from = releaseDir(cargoTargetDir);
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

const cargoTargetDir = targetDir();
build(cargoTargetDir);
bundle(cargoTargetDir);
console.log("bundle complete");
