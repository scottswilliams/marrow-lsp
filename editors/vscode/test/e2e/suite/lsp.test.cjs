// LSP transport: the packaged extension launches the bundled marrow-lsp and answers a
// live hover on a real .mw symbol. A handshake failure or a wrong-arch server binary
// makes the hover never arrive and this test fails.

const assert = require("node:assert");
const vscode = require("vscode");

const EXTENSION_ID = "marrow-lang.marrow";

async function activateMarrow() {
  const ext = vscode.extensions.getExtension(EXTENSION_ID);
  assert.ok(ext, `the installed extension ${EXTENSION_ID} is present`);
  await ext.activate();
  return ext;
}

async function retryHover(uri, position) {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const hovers = await vscode.commands.executeCommand("vscode.executeHoverProvider", uri, position);
    if (Array.isArray(hovers) && hovers.length > 0) {
      return hovers;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  return [];
}

suite("LSP transport", () => {
  test("hover on a .mw symbol returns Marrow facts from the bundled server", async () => {
    await activateMarrow();

    const folder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(folder, "the fixture project is open as a workspace folder");

    const fileUri = vscode.Uri.joinPath(folder.uri, "src", "shelf", "sample.mw");
    const doc = await vscode.workspace.openTextDocument(fileUri);
    await vscode.window.showTextDocument(doc);

    const text = doc.getText();
    const marker = "pub fn add(";
    const offset = text.indexOf(marker);
    assert.ok(offset >= 0, "the fixture declares the add function");
    // Point at the function name, just past "pub fn ".
    const position = doc.positionAt(offset + "pub fn ".length + 1);

    const hovers = await retryHover(fileUri, position);
    assert.ok(hovers.length > 0, "the bundled language server answered a hover request");
  });
});
