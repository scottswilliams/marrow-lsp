#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = resolve(here, "..");
const repoRoot = resolve(extensionDir, "..", "..");

const vscodeTargets = {
  darwin: {
    arm64: "darwin-arm64",
    x64: "darwin-x64",
  },
  linux: {
    arm64: "linux-arm64",
    x64: "linux-x64",
  },
  win32: {
    x64: "win32-x64",
  },
};

function command(name) {
  return process.platform === "win32" ? `${name}.cmd` : name;
}

function currentVscodeTarget() {
  const target = vscodeTargets[process.platform]?.[process.arch];
  if (target === undefined) {
    throw new Error(`Unsupported VS Code package target for ${process.platform}/${process.arch}.`);
  }
  return target;
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

function run(name, args, env) {
  console.log(`$ ${name} ${args.join(" ")}`);
  execFileSync(command(name), args, {
    cwd: extensionDir,
    env,
    stdio: "inherit",
  });
}

const env = {
  ...process.env,
  CARGO_TARGET_DIR: cargoTargetDir(),
};
const target = currentVscodeTarget();

run("npm", ["run", "compile:release"], env);
run("npm", ["run", "bundle"], env);
run("npm", ["run", "package:vsix", "--", "--target", target], env);
