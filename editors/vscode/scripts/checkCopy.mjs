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
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "F5 debug/admin entry prototype framing",
  /debug\/admin prototype[\s\S]*canonical\s+function-entry facts/i,
);
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "F5 entry string production caveat",
  /not a stable production entry API/i,
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
assertMentions(liveDataDescription, "marrow.liveData", "debug/admin setting framing", /debug\/admin/i);
assertMentions(liveDataDescription, "marrow.liveData", "presentation-only setting framing", /presentation-only/i);
assertMentions(liveDataDescription, "marrow.liveData", "advisory integrity framing", /advisory/i);
assertMentions(liveDataDescription, "marrow.liveData", "native dev-store reads", /native dev[- ]store/i);

const marrowDebugger = packageJson.contributes.debuggers.find(
  (debuggerContribution) => debuggerContribution.type === "marrow",
);
const launchProperties = marrowDebugger.configurationAttributes.launch.properties;
const entryDescription = launchProperties.entry.description;
assertMentions(
  entryDescription,
  "debugger entry",
  "debug/admin prototype framing",
  /debug\/admin prototype/i,
);
assertMentions(
  entryDescription,
  "debugger entry",
  "canonical function-entry facts",
  /canonical function-entry facts/i,
);
assertMentions(
  entryDescription,
  "debugger entry",
  "entry API caveat",
  /not a stable production entry API/i,
);

const debugSnippetDescription = marrowDebugger.configurationSnippets[0].description;
assertMentions(
  debugSnippetDescription,
  "debug snippet description",
  "debug/admin prototype framing",
  /debug\/admin prototype/i,
);
assertMentions(
  debugSnippetDescription,
  "debug snippet description",
  "canonical function-entry facts",
  /canonical function-entry facts/i,
);

const readme = files.get("README.md");
const readmeF5 = markdownBullet(readme, "README.md", "Debugging (F5)");
assertMentions(readme, "README.md", "rename catalog-evolution caveat", /catalog-backed evolution facts/i);
assertMentions(readme, "README.md", "debug/admin setting framing", /marrow\.liveData[\s\S]*debug\/admin/i);
assertMentions(readme, "README.md", "presentation-only setting framing", /marrow\.liveData[\s\S]*presentation-only/i);
assertMentions(readme, "README.md", "advisory setting framing", /marrow\.liveData[\s\S]*advisory/i);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "F5 debug/admin entry prototype framing",
  /debug\/admin prototype[\s\S]*canonical\s+function-entry facts/i,
);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "F5 entry string production caveat",
  /not a stable production entry API/i,
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
