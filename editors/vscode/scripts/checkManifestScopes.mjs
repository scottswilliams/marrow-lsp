// Trust-boundary lint for the extension manifest.
//
// The extension spawns whatever binary `marrow.server.path` / `marrow.dap.path` name.
// If those settings were window-scoped, a `.vscode/settings.json` committed to a repo
// could silently redirect the launched binary, so opening an untrusted folder would run
// an arbitrary executable. A machine-class scope pins the override to the user's machine
// settings, and listing the settings under `capabilities.untrustedWorkspaces` makes the
// override inert until the workspace is trusted. This lint fails the build if either
// guarantee regresses.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const extensionDir = dirname(dirname(fileURLToPath(import.meta.url)));
const manifest = JSON.parse(readFileSync(join(extensionDir, "package.json"), "utf8"));

const MACHINE_SCOPES = new Set(["machine", "machine-overridable"]);
const EXECUTABLE_PATH_SETTINGS = ["marrow.server.path", "marrow.dap.path"];

const properties = manifest.contributes?.configuration?.properties ?? {};
const failures = [];

for (const setting of EXECUTABLE_PATH_SETTINGS) {
  const definition = properties[setting];
  if (definition === undefined) {
    failures.push(`${setting} is not declared in contributes.configuration.properties`);
    continue;
  }
  if (!MACHINE_SCOPES.has(definition.scope)) {
    failures.push(
      `${setting} must have a machine-class scope (machine or machine-overridable), found ${JSON.stringify(definition.scope)}`,
    );
  }
}

const untrusted = manifest.capabilities?.untrustedWorkspaces;
if (untrusted === undefined) {
  failures.push("capabilities.untrustedWorkspaces must be declared so the trust boundary is explicit");
} else {
  const restricted = new Set(untrusted.restrictedConfigurations ?? []);
  for (const setting of EXECUTABLE_PATH_SETTINGS) {
    if (!restricted.has(setting)) {
      failures.push(
        `${setting} must appear in capabilities.untrustedWorkspaces.restrictedConfigurations so an untrusted workspace cannot apply it`,
      );
    }
  }
}

if (failures.length > 0) {
  throw new Error(`manifest scope lint failed:\n- ${failures.join("\n- ")}`);
}

assert.ok(true);
