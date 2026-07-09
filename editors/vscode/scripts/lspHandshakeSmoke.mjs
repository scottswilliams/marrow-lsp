#!/usr/bin/env node
// Native launch smoke for a freshly built marrow-lsp binary. Spawns the bundled server,
// runs one real LSP `initialize` handshake over stdio, asserts the marrow-lsp server
// answers, then sends `exit` and confirms a clean shutdown. This is the only release step
// that actually launches the per-platform binary, so it catches a wrong-arch build, a
// missing glibc floor, or an unsigned-darwin launch failure that copying the artifact
// cannot. Cross-compiled triples, whose binary cannot run on the build runner, are skipped
// by the workflow rather than faked here.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const extensionDir = resolve(here, "..");
const EXE = process.platform === "win32" ? ".exe" : "";

function resolveServerBinary() {
  const explicit = process.env.MARROW_LSP?.trim();
  if (explicit) {
    return explicit;
  }
  const bundled = join(extensionDir, "server", `marrow-lsp${EXE}`);
  if (existsSync(bundled)) {
    return bundled;
  }
  throw new Error(
    `could not find a bundled marrow-lsp binary at ${bundled}; run the bundle first or set MARROW_LSP`,
  );
}

// An LSP message with its Content-Length header, the framing the protocol mandates.
function frame(message) {
  const body = Buffer.from(JSON.stringify(message), "utf8");
  return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, "ascii"), body]);
}

// Pull the first complete LSP frame from a buffer, or undefined while it is still partial.
function takeFrame(buffer) {
  const headerEnd = buffer.indexOf("\r\n\r\n");
  if (headerEnd === -1) {
    return undefined;
  }
  const header = buffer.subarray(0, headerEnd).toString("ascii");
  const match = header.match(/Content-Length:\s*(\d+)/i);
  if (!match) {
    throw new Error(`LSP frame header without Content-Length: ${header}`);
  }
  const bodyStart = headerEnd + 4;
  const bodyLen = Number(match[1]);
  if (buffer.length < bodyStart + bodyLen) {
    return undefined;
  }
  const body = buffer.subarray(bodyStart, bodyStart + bodyLen).toString("utf8");
  return { message: JSON.parse(body), rest: buffer.subarray(bodyStart + bodyLen) };
}

function handshake(binary) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "inherit"] });
    let stdout = Buffer.alloc(0);
    let initialize;

    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error("marrow-lsp did not complete the initialize handshake within the timeout"));
    }, 30_000);
    timer.unref();

    child.on("error", reject);
    child.on("close", (code) => {
      clearTimeout(timer);
      resolvePromise({ initialize, code });
    });

    child.stdout.on("data", (chunk) => {
      stdout = Buffer.concat([stdout, chunk]);
      let taken;
      while ((taken = takeFrame(stdout)) !== undefined) {
        stdout = taken.rest;
        if (taken.message.id === 1) {
          initialize = taken.message;
          // Handshake answered; ask for a clean teardown and let the process close.
          child.stdin.write(frame({ jsonrpc: "2.0", method: "exit" }));
        }
      }
    });

    child.stdin.write(
      frame({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: { processId: process.pid, rootUri: null, capabilities: {} },
      }),
    );
  });
}

async function main() {
  const binary = resolveServerBinary();
  const { initialize, code } = await handshake(binary);
  assert.ok(initialize, "the server answered initialize");
  assert.equal(
    initialize.result.serverInfo.name,
    "marrow-lsp",
    "initialize identifies the marrow-lsp server",
  );
  assert.equal(code, 0, "the server exited cleanly after `exit`");
  console.log(`LSP native handshake: initialize + exit verified against ${binary}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
