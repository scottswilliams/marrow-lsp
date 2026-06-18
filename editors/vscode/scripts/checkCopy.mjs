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
    phrase: ["live data", "values", "read from the durable store"].join(" "),
  },
  {
    file: "CHANGELOG.md",
    phrase: ["Record-count", "Code", "Lens backed by stored data"].join(" "),
  },
  {
    file: "CHANGELOG.md",
    phrase: "browsing the project's durable data",
  },
  {
    file: "CHANGELOG.md",
    phrase: "Marrow Data Roots",
  },
  {
    file: "README.md",
    phrase: "Marrow Data Roots",
  },
  {
    file: "README.md",
    phrase: "query-native",
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
  {
    file: "README.md",
    phrase: "Hover — type and documentation from canonical analysis facts",
  },
  {
    file: "CHANGELOG.md",
    phrase: "Hover with type and documentation from canonical analysis facts",
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

function assertDoesNotMention(text, source, label, pattern) {
  assert.doesNotMatch(text, pattern, `${source} must not mention ${label}`);
}

function markdownBullet(text, source, marker) {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((line) => line.startsWith("- ") && line.includes(marker));
  assert.notEqual(start, -1, `${source} must include a bullet for ${marker}`);

  const block = [];
  for (let index = start; index < lines.length; index += 1) {
    const line = lines[index];
    if (index !== start && (line.startsWith("- ") || line.startsWith("## "))) {
      break;
    }
    block.push(line);
  }
  return block.join("\n");
}

const changelog = files.get("CHANGELOG.md");
const changelogF5 = markdownBullet(changelog, "CHANGELOG.md", "F5 debugging");
assertMentions(
  changelog,
  "CHANGELOG.md",
  "presentation-only source intelligence framing",
  /Presentation-only source-intelligence helpers[\s\S]*not stable production semantic contracts/i,
);
assertMentions(
  changelog,
  "CHANGELOG.md",
  "Saved Resource Inspector framing",
  /Saved Resource Inspector view for opt-in committed saved-resource inspection[\s\S]*Marrow-owned store\s+facts/i,
);
assertMentions(
  changelog,
  "CHANGELOG.md",
  "live-data facts framing",
  /Marrow-owned store\s+facts/i,
);
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "F5 canonical entry facts framing",
  /canonical\s+function-entry\s+facts/i,
);
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "default entry framing",
  /defaultEntry/i,
);
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "explicit entry blocked framing",
  /explicit\s+entry\s+strings[\s\S]*blocked/i,
);
assertDoesNotMention(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "entry placeholder",
  /module::function/i,
);
assertDoesNotMention(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "disabled F5 debugging",
  /\b(disabled|unavailable)\b/i,
);

const liveDataDescription =
  packageJson.contributes.configuration.properties["marrow.liveData"].markdownDescription;
assertMentions(liveDataDescription, "marrow.liveData", "opt-in setting framing", /opt[- ]in/i);
assertMentions(liveDataDescription, "marrow.liveData", "advisory integrity framing", /advisory/i);
assertMentions(liveDataDescription, "marrow.liveData", "native dev-store reads", /native dev[- ]store/i);

const marrowDebugger = packageJson.contributes.debuggers.find(
  (debuggerContribution) => debuggerContribution.type === "marrow",
);
const marrowDataView = packageJson.contributes.views.marrow.find(
  (view) => view.id === "marrowData",
);
assert.equal(
  marrowDataView.name,
  "Saved Resource Inspector",
  "saved-resource view title must name the inspector",
);
const launchProperties = marrowDebugger.configurationAttributes.launch.properties;
assert.equal(
  Object.hasOwn(launchProperties, "entry"),
  false,
  "debug launch configuration must not expose explicit entry strings",
);
assert.equal(
  launchProperties.stopOnEntry.default,
  true,
  "debug launch configuration must default to stop-on-entry while line breakpoints are advisory",
);
assert.equal(
  marrowDebugger.initialConfigurations[0].stopOnEntry,
  true,
  "generated debug configuration must stop on entry by default",
);

const debugSnippetDescription = marrowDebugger.configurationSnippets[0].description;
assertMentions(
  debugSnippetDescription,
  "debug snippet description",
  "canonical function-entry facts",
  /canonical function-entry facts/i,
);
const debugSnippetBody = marrowDebugger.configurationSnippets[0].body;
assert.equal(
  Object.hasOwn(debugSnippetBody, "entry"),
  false,
  "debug snippet must not seed a blocked entry string",
);
assert.equal(
  debugSnippetBody.stopOnEntry,
  true,
  "debug snippet must stop on entry by default",
);
assertMentions(
  debugSnippetBody.name,
  "debug snippet name",
  "default entry framing",
  /default/i,
);

const readme = files.get("README.md");
const readmeF5 = markdownBullet(readme, "README.md", "Debugging (F5)");
const readmeSourceIntelligence = markdownBullet(readme, "README.md", "Source intelligence");
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "presentation-only source intelligence framing",
  /presentation-only[\s\S]*not stable production semantic contracts/i,
);
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "Marrow-owned typed facts",
  /typed hover\/navigation\/completion facts stay owned by Marrow/i,
);
assertMentions(readme, "README.md", "rename catalog-evolution caveat", /catalog-backed evolution facts/i);
assertMentions(readme, "README.md", "advisory setting framing", /marrow\.liveData[\s\S]*advisory/i);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "F5 canonical entry facts framing",
  /canonical\s+function-entry\s+facts/i,
);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "default entry framing",
  /defaultEntry/i,
);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "explicit entry blocked framing",
  /explicit\s+entry\s+strings[\s\S]*blocked/i,
);
assertDoesNotMention(
  readmeF5,
  "README.md F5 bullet",
  "entry placeholder",
  /module::function/i,
);
assertDoesNotMention(
  readmeF5,
  "README.md F5 bullet",
  "disabled F5 debugging",
  /\b(disabled|unavailable)\b/i,
);

const dataIntegrity = files.get("src/dataIntegrity.ts");
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "advisory framing", /Advisory/i);
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "opt-in live-data framing", /opt-in live data/i);
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "native dev-store framing", /native dev-store/i);
assertMentions(dataIntegrity, "src/dataIntegrity.ts", "advisory findings", /advisory issue\(s\) found/i);
