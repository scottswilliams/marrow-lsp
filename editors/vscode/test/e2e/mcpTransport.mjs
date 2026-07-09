// MCP transport end-to-end. The MCP server is not bundled in the VSIX (coding agents,
// not the editor, launch it), so it is driven here directly: spawn the real marrow-mcp
// binary and speak newline-delimited JSON-RPC to it, initializing, listing tools, and
// calling mw_check on a snippet. A handshake or dispatch failure fails the job.
//
// The binary path comes from MARROW_MCP, else the release/debug build under
// CARGO_TARGET_DIR (or the repo-local target as a last resort for a plain checkout).

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..", "..", "..");
const EXE = process.platform === "win32" ? ".exe" : "";

function resolveMcpBinary() {
  const explicit = process.env.MARROW_MCP?.trim();
  if (explicit) {
    return explicit;
  }
  const targetRoot = process.env.CARGO_TARGET_DIR?.trim() || join(repoRoot, "target");
  const triple = process.env.CARGO_BUILD_TARGET?.trim();
  const bases = triple ? [join(targetRoot, triple)] : [targetRoot];
  for (const base of bases) {
    for (const profile of ["release", "debug"]) {
      const candidate = join(base, profile, `marrow-mcp${EXE}`);
      if (existsSync(candidate)) {
        return candidate;
      }
    }
  }
  throw new Error("could not find a built marrow-mcp binary; set MARROW_MCP or build it first");
}

// Drive a set of JSON-RPC requests over the server's stdio, collecting one reply line
// per request that carries an id. Notifications (no id) get no reply.
function driveMcp(binary, requests) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "inherit"] });
    const replies = [];
    const expected = requests.filter((request) => request.id !== undefined).length;

    const rl = createInterface({ input: child.stdout });
    rl.on("line", (line) => {
      const trimmed = line.trim();
      if (trimmed.length === 0) {
        return;
      }
      replies.push(JSON.parse(trimmed));
      if (replies.length >= expected) {
        child.stdin.end();
      }
    });

    child.on("error", reject);
    child.on("close", () => resolvePromise(replies));

    for (const request of requests) {
      child.stdin.write(`${JSON.stringify(request)}\n`);
    }

    setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error("marrow-mcp did not answer within the timeout"));
    }, 30_000).unref();
  });
}

async function main() {
  const binary = resolveMcpBinary();
  const replies = await driveMcp(binary, [
    { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18" } },
    { jsonrpc: "2.0", method: "notifications/initialized" },
    { jsonrpc: "2.0", id: 2, method: "tools/list" },
    {
      jsonrpc: "2.0",
      id: 3,
      method: "tools/call",
      params: { name: "mw_check", arguments: { source: "module a\n\npub fn broken(:\n" } },
    },
  ]);

  const byId = new Map(replies.map((reply) => [reply.id, reply]));

  const initialize = byId.get(1);
  assert.ok(initialize, "the server answered initialize");
  assert.equal(initialize.result.serverInfo.name, "marrow-mcp", "initialize identifies the marrow MCP server");
  assert.equal(initialize.result.protocolVersion, "2025-06-18", "the requested protocol version is echoed");

  const toolsList = byId.get(2);
  assert.ok(toolsList, "the server answered tools/list");
  const toolNames = toolsList.result.tools.map((tool) => tool.name);
  assert.ok(toolNames.includes("mw_check"), "tools/list advertises mw_check");

  const check = byId.get(3);
  assert.ok(check, "the server answered tools/call mw_check");
  assert.equal(check.result.isError, false, "a check that finds diagnostics is not a tool error");
  const diagnostics = check.result.structuredContent.diagnostics;
  assert.ok(Array.isArray(diagnostics) && diagnostics.length > 0, "the real checker reported a diagnostic");

  console.log(`MCP transport: initialize + tools/list + mw_check verified against ${binary}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
