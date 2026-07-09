// DAP transport: the packaged extension launches the bundled marrow-dap and drives a
// launch with stop-on-entry, reaching a live stopped session, then tears it down. A
// broken adapter handshake means startDebugging never yields an active session.

const assert = require("node:assert");
const vscode = require("vscode");

async function waitFor(predicate, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) {
      return true;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  return predicate();
}

suite("DAP transport", () => {
  test("launch with stopOnEntry reaches an active session and terminates cleanly", async () => {
    const folder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(folder, "the fixture project is open as a workspace folder");

    let terminated = false;
    const subscription = vscode.debug.onDidTerminateDebugSession(() => {
      terminated = true;
    });

    try {
      const started = await vscode.debug.startDebugging(folder, {
        type: "marrow",
        request: "launch",
        name: "e2e default entry",
        project: folder.uri.fsPath,
        stopOnEntry: true,
      });
      assert.ok(started, "the bundled debug adapter accepted the launch");

      const active = await waitFor(() => vscode.debug.activeDebugSession !== undefined, 8_000);
      assert.ok(active, "a debug session became active (stopped on entry)");

      await vscode.debug.stopDebugging();
      const ended = await waitFor(() => terminated, 8_000);
      assert.ok(ended, "the debug session terminated after stop");
    } finally {
      subscription.dispose();
    }
  });
});
