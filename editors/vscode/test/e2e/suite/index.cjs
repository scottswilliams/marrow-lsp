// Mocha entry point loaded inside the VS Code extension host. It collects every
// *.test.cjs sibling and runs them, rejecting if any fail so the CI job fails.

const fs = require("node:fs");
const path = require("node:path");
const Mocha = require("mocha");

function run() {
  const mocha = new Mocha({ ui: "tdd", color: true, timeout: 90_000 });
  const suiteDir = __dirname;
  for (const file of fs.readdirSync(suiteDir)) {
    if (file.endsWith(".test.cjs")) {
      mocha.addFile(path.join(suiteDir, file));
    }
  }

  return new Promise((resolve, reject) => {
    mocha.run((failures) => {
      if (failures > 0) {
        reject(new Error(`${failures} e2e test(s) failed`));
      } else {
        resolve();
      }
    });
  });
}

module.exports = { run };
