import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const extensionDir = dirname(scriptDir);
const repoRoot = resolve(extensionDir, "..", "..");
const bundlePath = join(scriptDir, "bundle.mjs");
const packageScriptPath = join(scriptDir, "package.mjs");
const packageJsonPath = join(extensionDir, "package.json");
const packageLockPath = join(extensionDir, "package-lock.json");
const readmePath = join(extensionDir, "README.md");

const bundleScript = readFileSync(bundlePath, "utf8");
const packageScript = readFileSync(packageScriptPath, "utf8");
const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8"));
const packageLock = JSON.parse(readFileSync(packageLockPath, "utf8"));
const readme = readFileSync(readmePath, "utf8");
const failures = [];

function check(label, test) {
  try {
    test();
  } catch (error) {
    failures.push(`${label}: ${error.message}`);
  }
}

function runBundleWithoutTargetDirRejectsBeforeCargo() {
  const stubDir = mkdtempSync(join(tmpdir(), "marrow-vscode-bundle-missing-target-"));
  const cargoPath = join(stubDir, process.platform === "win32" ? "cargo.cmd" : "cargo");
  const cargoMarker = join(stubDir, "cargo-invoked");
  const cargoScript =
    process.platform === "win32"
      ? `@echo off\r\necho invoked > "${cargoMarker}"\r\nexit /b 86\r\n`
      : `#!/bin/sh\necho invoked > "${cargoMarker}"\nexit 86\n`;

  try {
    writeFileSync(cargoPath, cargoScript);
    chmodSync(cargoPath, 0o755);

    const env = {
      ...process.env,
      PATH: `${stubDir}${delimiter}${process.env.PATH ?? ""}`,
    };
    delete env.CARGO_TARGET_DIR;
    delete env.CARGO_BUILD_TARGET;

    assert.throws(
      () => execFileSync(process.execPath, [bundlePath], { cwd: extensionDir, env, stdio: "pipe" }),
      (error) => {
        assert.equal(
          existsSync(cargoMarker),
          false,
          "bundle smoke must validate CARGO_TARGET_DIR before invoking cargo",
        );
        assert.match(
          Buffer.concat([error.stdout ?? Buffer.alloc(0), error.stderr ?? Buffer.alloc(0)]).toString(),
          /CARGO_TARGET_DIR/,
          "bundle smoke failure without CARGO_TARGET_DIR must explain the required variable",
        );
        return true;
      },
      "bundle smoke must fail before cargo when CARGO_TARGET_DIR is unset",
    );
  } finally {
    rmSync(stubDir, { recursive: true, force: true });
  }
}

function runBundleWithStubCargoUsesExplicitTargetDir() {
  const stubDir = mkdtempSync(join(tmpdir(), "marrow-vscode-bundle-cargo-"));
  const targetDir = mkdtempSync(join(tmpdir(), "marrow-vscode-target-"));
  const recordPath = join(stubDir, "cargo-record.json");
  const cargoStubPath = join(stubDir, "cargo-stub.cjs");
  const cargoPath = join(stubDir, process.platform === "win32" ? "cargo.cmd" : "cargo");
  const exe = process.platform === "win32" ? ".exe" : "";
  const cargoBuildTarget = "probe-target";
  const serverDir = join(extensionDir, "server");

  const cargoStub = `
const fs = require("node:fs");
const path = require("node:path");
const recordPath = ${JSON.stringify(recordPath)};
const exe = ${JSON.stringify(exe)};
const targetDir = process.env.CARGO_TARGET_DIR;
const cargoBuildTarget = process.env.CARGO_BUILD_TARGET;
fs.writeFileSync(recordPath, JSON.stringify({
  argv: process.argv.slice(2),
  cargoTargetDir: targetDir,
  cargoBuildTarget,
}, null, 2));
const releaseDir = path.join(targetDir, cargoBuildTarget, "release");
fs.mkdirSync(releaseDir, { recursive: true });
for (const name of ["marrow-lsp", "marrow-dap"]) {
  fs.writeFileSync(path.join(releaseDir, name + exe), "from-explicit-target:" + name);
}
`;
  const cargoScript =
    process.platform === "win32"
      ? `@echo off\r\nnode "${cargoStubPath}" %*\r\n`
      : `#!/bin/sh\nexec node "${cargoStubPath}" "$@"\n`;

  try {
    writeFileSync(cargoStubPath, cargoStub);
    writeFileSync(cargoPath, cargoScript);
    chmodSync(cargoPath, 0o755);
    rmSync(serverDir, { recursive: true, force: true });

    execFileSync(process.execPath, [bundlePath], {
      cwd: extensionDir,
      env: {
        ...process.env,
        CARGO_BUILD_TARGET: cargoBuildTarget,
        CARGO_TARGET_DIR: targetDir,
        PATH: `${stubDir}${delimiter}${process.env.PATH ?? ""}`,
      },
      stdio: "pipe",
    });

    const record = JSON.parse(readFileSync(recordPath, "utf8"));
    assert.deepEqual(record.argv.slice(0, 3), [
      "build",
      "--manifest-path",
      join(repoRoot, "Cargo.toml"),
    ]);
    assert.ok(record.argv.includes("--release"), "cargo argv includes --release");
    assert.ok(record.argv.includes("--target"), "cargo argv includes --target");
    assert.ok(record.argv.includes(cargoBuildTarget), "cargo argv includes CARGO_BUILD_TARGET");
    assert.equal(record.cargoTargetDir, targetDir, "cargo receives the validated CARGO_TARGET_DIR");
    assert.equal(record.cargoBuildTarget, cargoBuildTarget, "cargo receives CARGO_BUILD_TARGET");

    for (const name of ["marrow-lsp", "marrow-dap"]) {
      assert.equal(
        readFileSync(join(serverDir, `${name}${exe}`), "utf8"),
        `from-explicit-target:${name}`,
        `${name} must be copied from the explicit CARGO_TARGET_DIR release directory`,
      );
    }
  } finally {
    rmSync(stubDir, { recursive: true, force: true });
    rmSync(targetDir, { recursive: true, force: true });
    rmSync(serverDir, { recursive: true, force: true });
  }
}

check("bundle script carries explicit cargo contract", () => {
  assert.match(
    bundleScript,
    /"--manifest-path"/,
    "bundle smoke cargo build must pass an explicit --manifest-path",
  );
  assert.match(
    bundleScript,
    /"Cargo\.toml"/,
    "bundle smoke cargo build must point at this worktree's Cargo.toml",
  );
  assert.doesNotMatch(
    bundleScript,
    /join\(repoRoot,\s*"target"\)/,
    "bundle smoke must not fall back to the repo-local target directory",
  );
  assert.match(
    bundleScript,
    /CARGO_TARGET_DIR[\s\S]*outside/,
    "bundle smoke must require an explicit outside-repo CARGO_TARGET_DIR",
  );
});

check("bundle rejects missing CARGO_TARGET_DIR before cargo", runBundleWithoutTargetDirRejectsBeforeCargo);
check("bundle uses explicit target dir at runtime", runBundleWithStubCargoUsesExplicitTargetDir);

check("vsce packager is installed through npm ci", () => {
  assert.equal(packageJson.scripts["package:vsix"], "vsce package --no-dependencies");
  assert.match(
    packageJson.devDependencies["@vscode/vsce"],
    /^\d+\.\d+\.\d+$/,
    "VSIX packager must be an explicit, reviewed dev dependency",
  );
  assert.equal(
    packageLock.packages[""].devDependencies["@vscode/vsce"],
    packageJson.devDependencies["@vscode/vsce"],
  );
  assert.ok(packageLock.packages["node_modules/@vscode/vsce"], "package-lock must pin @vscode/vsce");
});

check("package command is portable", () => {
  assert.equal(packageJson.scripts.package, "node ./scripts/package.mjs");
  assert.doesNotMatch(packageJson.scripts.package, /\$\(/, "package script must not use POSIX substitution");
  assert.match(
    packageScript,
    /run\("npm", \["run", "package:vsix", "--", "--target", target\], env\)/,
    "package helper must use the locally installed VSIX packager",
  );
  assert.doesNotMatch(packageScript, /\bnpx\b/, "package helper must not invoke network-backed npx");
});

check("package contribution titles are bounded", () => {
  const refresh = packageJson.contributes.commands.find((command) => command.command === "marrow.refreshData");
  assert.equal(refresh?.title, "Refresh Data Roots");
});

check("README development docs are cross-platform honest", () => {
  assert.match(
    readme,
    /Set `CARGO_TARGET_DIR` in your environment before running `npm run bundle` or `npm run package`/i,
  );
  assert.doesNotMatch(
    readme,
    /^CARGO_TARGET_DIR=.*npm run (?:bundle|package)/m,
    "README must not present POSIX env assignment as the only package path",
  );
  assert.doesNotMatch(readme, /\/tmp\/marrow-vscode-target/, "README must not use POSIX-only /tmp examples");
});

if (failures.length > 0) {
  throw new Error(`bundle/package smoke check failed:\n- ${failures.join("\n- ")}`);
}
