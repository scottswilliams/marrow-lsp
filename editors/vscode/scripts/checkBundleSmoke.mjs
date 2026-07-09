import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
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
  // Cross-compilation contract: the release build honors a per-triple CARGO_BUILD_TARGET
  // and passes --target so one runner can build another platform's binary. (A real
  // per-platform launch is exercised by the packaged-extension e2e, not fake bytes here.)
  assert.match(bundleScript, /CARGO_BUILD_TARGET/, "bundle must honor a per-triple CARGO_BUILD_TARGET");
  assert.match(bundleScript, /"--target"/, "bundle must pass --target for a cross-compiled triple");
});

check("bundle rejects missing CARGO_TARGET_DIR before cargo", runBundleWithoutTargetDirRejectsBeforeCargo);

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
    /run\("npm", \["run", "package:vsix", "--", "--target", vscodeTarget\], env\)/,
    "package helper must use the locally installed VSIX packager",
  );
  assert.doesNotMatch(packageScript, /\bnpx\b/, "package helper must not invoke network-backed npx");
});

check("package guards reproducibility of a published VSIX", () => {
  assert.match(
    packageScript,
    /assertReproducibleUpstream/,
    "package helper must refuse a publishable VSIX built against upstream path dependencies",
  );
  assert.ok(
    packageScript.includes('path\\s*=\\s*"\\.\\.\\/marrow'),
    "the reproducibility guard must detect local ../marrow path dependencies",
  );
});

check("package contribution titles are bounded", () => {
  const refresh = packageJson.contributes.commands.find((command) => command.command === "marrow.refreshData");
  assert.equal(refresh?.title, "Refresh Saved Data");
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

check("marketplace governance metadata is present", () => {
  // A verified org publisher, not the individual scaffold account, so the listing is
  // owned by the project rather than one person's bus factor.
  assert.notEqual(
    packageJson.publisher,
    "scottswilliams",
    "publisher must be the org publisher, not the personal placeholder account",
  );
  assert.match(packageJson.publisher, /^[a-z0-9][a-z0-9-]*$/, "publisher must be a valid marketplace id");

  // A top-level gallery icon the marketplace can render, present on disk and a PNG.
  assert.ok(typeof packageJson.icon === "string" && packageJson.icon.length > 0, "a top-level gallery icon is declared");
  const iconPath = join(extensionDir, packageJson.icon);
  assert.ok(existsSync(iconPath), `gallery icon exists on disk: ${packageJson.icon}`);
  assert.equal(
    readFileSync(iconPath).subarray(1, 4).toString("ascii"),
    "PNG",
    "gallery icon must be a PNG (the marketplace does not accept SVG gallery icons)",
  );

  // The trust boundary is declared for both workspace-trust and virtual-workspace support.
  assert.ok(packageJson.capabilities, "a capabilities block is declared");
  assert.ok(
    packageJson.capabilities.untrustedWorkspaces,
    "capabilities.untrustedWorkspaces states the workspace-trust boundary",
  );
  assert.ok(
    packageJson.capabilities.virtualWorkspaces,
    "capabilities.virtualWorkspaces states the virtual-workspace support level",
  );
});

if (failures.length > 0) {
  throw new Error(`bundle/package smoke check failed:\n- ${failures.join("\n- ")}`);
}
