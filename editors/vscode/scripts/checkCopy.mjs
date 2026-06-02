import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const extensionDir = dirname(scriptDir);

function readExtensionFile(path) {
  return readFileSync(join(extensionDir, path), "utf8");
}

const files = new Map([
  ["CHANGELOG.md", readExtensionFile("CHANGELOG.md")],
  ["package.json", readExtensionFile("package.json")],
  ["README.md", readExtensionFile("README.md")],
  ["src/dataIntegrity.ts", readExtensionFile("src/dataIntegrity.ts")],
]);
const packageJson = JSON.parse(files.get("package.json"));

const forbiddenPhrases = [
  {
    file: "CHANGELOG.md",
    phrase: "live data values read from the durable store",
  },
  {
    file: "CHANGELOG.md",
    phrase: "Record-count CodeLens backed by stored data",
  },
  {
    file: "CHANGELOG.md",
    phrase: "browsing the project's durable data",
  },
  {
    file: "README.md",
    phrase: "renames never silently break durable data",
  },
  {
    file: "README.md",
    phrase: "persisted store schema",
  },
  {
    file: "src/dataIntegrity.ts",
    phrase: "No issues: stored data matches the schema",
  },
];

for (const { file, phrase } of forbiddenPhrases) {
  assert.equal(
    files.get(file).includes(phrase),
    false,
    `${file} must not contain stale production-data copy: ${phrase}`,
  );
}

function assertMentions(text, source, label, pattern) {
  assert.match(text, pattern, `${source} must mention ${label}`);
}

const changelog = files.get("CHANGELOG.md");
assertMentions(
  changelog,
  "CHANGELOG.md",
  "debug/admin live-data framing",
  /opt-in debug\/admin live data presentation[\s\S]*typed\/catalog-bound facts/i,
);
assertMentions(
  changelog,
  "CHANGELOG.md",
  "presentation-only CodeLens framing",
  /Presentation-only record-count CodeLens/i,
);
assertMentions(
  changelog,
  "CHANGELOG.md",
  "debug/admin Data Explorer framing",
  /Data Explorer view for opt-in debug\/admin inspection[\s\S]*typed\/catalog-bound store facts/i,
);

const liveDataDescription =
  packageJson.contributes.configuration.properties["marrow.liveData"].markdownDescription;
assertMentions(liveDataDescription, "marrow.liveData", "opt-in setting framing", /opt[- ]in/i);
assertMentions(liveDataDescription, "marrow.liveData", "debug/admin setting framing", /debug\/admin/i);
assertMentions(liveDataDescription, "marrow.liveData", "presentation-only setting framing", /presentation-only/i);
assertMentions(liveDataDescription, "marrow.liveData", "advisory integrity framing", /advisory/i);
assertMentions(liveDataDescription, "marrow.liveData", "native dev-store reads", /native dev[- ]store/i);

const readme = files.get("README.md");
assertMentions(readme, "README.md", "rename catalog-evolution caveat", /catalog-backed evolution facts/i);
assertMentions(readme, "README.md", "debug/admin setting framing", /marrow\.liveData[\s\S]*debug\/admin/i);
assertMentions(readme, "README.md", "presentation-only setting framing", /marrow\.liveData[\s\S]*presentation-only/i);
assertMentions(readme, "README.md", "advisory setting framing", /marrow\.liveData[\s\S]*advisory/i);

const dataIntegrity = files.get("src/dataIntegrity.ts");
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "advisory framing", /Advisory/i);
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "opt-in live-data framing", /opt-in live data/i);
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "native dev-store framing", /native dev-store/i);
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "advisory findings", /advisory issue\(s\) found/i);
