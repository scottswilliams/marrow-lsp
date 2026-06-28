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
  ["src/extension.ts", readExtensionFile("src/extension.ts")],
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

function assertOrderedSourceCalls(text, source, label, calls) {
  let cursor = -1;
  for (const call of calls) {
    const index = text.indexOf(call);
    assert.notEqual(index, -1, `${source} must call ${call} in ${label}`);
    assert.ok(index > cursor, `${source} must call ${call} after the previous ${label} call`);
    cursor = index;
  }
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
const changelogSavedInspector = markdownBullet(
  changelog,
  "CHANGELOG.md",
  "Saved Resource Inspector",
);
const changelogSourceIntelligence = markdownBullet(
  changelog,
  "CHANGELOG.md",
  "Source-intelligence helpers",
);
const readme = files.get("README.md");
const readmeSavedInspector = markdownBullet(readme, "README.md", "Saved Resource Inspector");
const readmeAttachLimitation = markdownBullet(
  readme,
  "README.md",
  "Attaching to programs",
);
assertMentions(
  changelogSourceIntelligence,
  "CHANGELOG.md source intelligence bullet",
  "fact-backed completion framing",
  /completion are backed by Marrow facts/i,
);
assertMentions(
  changelogSourceIntelligence,
  "CHANGELOG.md source intelligence bullet",
  "catalog navigation ready contract",
  /catalog-backed\s+go-to\s+definition\s+and\s+find\s+references[\s\S]*Marrow\s+navigation\s+facts/i,
);
assertDoesNotMention(
  changelogSourceIntelligence,
  "CHANGELOG.md source intelligence bullet",
  "store resource leaf graduation",
  /store\s+catalog\s+leaves/i,
);
assertMentions(
  changelogSourceIntelligence,
  "CHANGELOG.md source intelligence bullet",
  "editor aid caveat",
  /Hover\s+and\s+remaining\s+navigation\s+helpers\s+stay\s+editor\s+aids/i,
);
assertDoesNotMention(
  changelog,
  "CHANGELOG.md",
  "undifferentiated navigation caveat",
  /go-to\s+definition,\s+find\s+references,\s+and\s+rename\s+remain\s+editor\s+aids/i,
);
assertMentions(
  changelogSourceIntelligence,
  "CHANGELOG.md source intelligence bullet",
  "source-only rename fact framing",
  /Source-only\s+prepare\s+rename\s+and\s+editor\s+rename\s+edits\s+are\s+backed\s+by\s+Marrow\s+rename\s+facts/i,
);
assertMentions(
  changelogSourceIntelligence,
  "CHANGELOG.md source intelligence bullet",
  "remaining rename application caveat",
  /saved-data-backed\s+rename\s+application[\s\S]*remaining\s+hover\/navigation\s+facts stay owned by Marrow/i,
);
assertMentions(
  readme,
  "README.md",
  "catalog navigation ready contract",
  /catalog-backed\s+go-to\s+definition\s+and\s+find\s+references[\s\S]*Marrow\s+navigation\s+facts/i,
);
assertDoesNotMention(
  readme,
  "README.md",
  "undifferentiated navigation caveat",
  /go-to\s+definition,\s+find\s+references,\s+and\s+rename\s+remain\s+editor\s+aids/i,
);
assertMentions(
  changelog,
  "CHANGELOG.md",
  "Saved Resource Inspector framing",
  /Saved Resource Inspector view[\s\S]*production\s+read-only\s+data-view\s+contract[\s\S]*Marrow-owned store\s+facts/i,
);
assertMentions(
  changelogSavedInspector,
  "CHANGELOG.md Saved Resource Inspector bullet",
  "production read-only data-view contract",
  /production\s+read-only\s+data-view\s+contract/i,
);
assertMentions(
  changelogSavedInspector,
  "CHANGELOG.md Saved Resource Inspector bullet",
  "LSP-supplied automatic refresh scope",
  /automatically\s+refreshes[\s\S]*LSP-supplied\s+native\s+dev-store[\s\S]*committed-lock\s+watch\s+targets[\s\S]*marrow\.liveData/i,
);
assertMentions(
  changelogSavedInspector,
  "CHANGELOG.md Saved Resource Inspector bullet",
  "snapshot refusal",
  /refuses\s+to\s+mix[\s\S]*store\s+snapshots/i,
);
assertMentions(
  changelogSavedInspector,
  "CHANGELOG.md Saved Resource Inspector bullet",
  "no live watches",
  /does\s+not\s+watch[\s\S]*uncommitted\s+writes[\s\S]*debuggee\s+state[\s\S]*served\s+programs/i,
);
assertMentions(
  readmeSavedInspector,
  "README.md Saved Resource Inspector bullet",
  "production read-only data-view contract",
  /production\s+read-only\s+data-view\s+contract/i,
);
assertMentions(
  readmeSavedInspector,
  "README.md Saved Resource Inspector bullet",
  "LSP-supplied automatic refresh scope",
  /automatically\s+refreshes[\s\S]*LSP-supplied\s+native\s+dev-store[\s\S]*committed-lock\s+watch\s+targets[\s\S]*marrow\.liveData/i,
);
assertMentions(
  readmeSavedInspector,
  "README.md Saved Resource Inspector bullet",
  "snapshot refusal",
  /refuses\s+to\s+mix[\s\S]*different\s+snapshot/i,
);
assertDoesNotMention(
  files.get("src/extension.ts"),
  "src/extension.ts",
  "Saved Resource Inspector auto-refresh on saved documents",
  /onDidSaveTextDocument[\s\S]*dataProvider\.refresh\(\)/,
);
assertDoesNotMention(
  files.get("src/extension.ts"),
  "src/extension.ts",
  "direct publishDiagnostics subscription",
  /onNotification\(\s*["']textDocument\/publishDiagnostics["']/,
);
const diagnosticsMiddleware = files
  .get("src/extension.ts")
  .match(/handleDiagnostics\(\s*uri,\s*diagnostics,\s*next\s*\)\s*\{([\s\S]*?)\n\s*\},/);
assert.notEqual(
  diagnosticsMiddleware,
  null,
  "src/extension.ts must use diagnostics middleware for publishDiagnostics side effects",
);
assertOrderedSourceCalls(
  diagnosticsMiddleware[1],
  "src/extension.ts diagnostics middleware",
  "diagnostics middleware",
  ["next(uri, diagnostics);", "refreshWatchTargets();", "dataProvider.refresh();"],
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
  "typed debug args framing",
  /typed\s+`args`/i,
);
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "exact scalar debug args framing",
  /exact\s+string\s+forms/i,
);
assertMentions(
  changelogF5,
  "CHANGELOG.md F5 bullet",
  "explicit entry framing",
  /explicit\s+`entry`/i,
);
assertDoesNotMention(
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
assertMentions(
  liveDataDescription,
  "marrow.liveData",
  "production read-only data-view contract",
  /production\s+read-only\s+data-view\s+contract/i,
);
assertDoesNotMention(liveDataDescription, "marrow.liveData", "retired advisory framing", /advisory/i);
assertDoesNotMention(liveDataDescription, "marrow.liveData", "retired integrity framing", /integrity/i);
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
assert.equal(
  Object.hasOwn(marrowDebugger.configurationAttributes, "attach"),
  false,
  "VS Code must not advertise attach until Marrow exposes served-process control facts",
);
assertMentions(
  readmeAttachLimitation,
  "README.md attach limitation",
  "unsupported attach framing",
  /attach[\s\S]*unsupported/i,
);
assertMentions(
  readmeAttachLimitation,
  "README.md attach limitation",
  "served-process control boundary facts",
  /served-process\s+control\s+boundary\s+facts/i,
);
assertDoesNotMention(
  readmeAttachLimitation,
  "README.md attach limitation",
  "blocked attach contract",
  /blocked\s+Marrow\s+contract/i,
);
assertDoesNotMention(
  readmeAttachLimitation,
  "README.md attach limitation",
  "generic serve boundary blocker",
  /serve\/attach\s+execution\s+boundary\s+facts/i,
);
const launchProperties = marrowDebugger.configurationAttributes.launch.properties;
assert.equal(
  Object.hasOwn(launchProperties, "entry"),
  true,
  "debug launch configuration should expose Marrow-admitted explicit entry selectors",
);
assert.match(
  launchProperties.entry.description,
  /defaultEntry/i,
  "debug launch entry description must explain defaultEntry fallback",
);
assert.equal(
  launchProperties.args.maxItems,
  undefined,
  "debug launch args must not be capped now that DAP accepts typed args",
);
assert.match(
  launchProperties.args.description,
  /typed/i,
  "debug launch args description must name typed args",
);
assert.deepEqual(
  launchProperties.args.items,
  { $ref: "#/$defs/entryArgument" },
  "debug launch args should use the single recursive entryArgument schema",
);
const launchDefs = marrowDebugger.configurationAttributes.launch.$defs;
assert.equal(
  launchDefs.entryArgument.additionalProperties,
  false,
  "debug launch arg objects must be closed",
);
assert.equal(
  launchDefs.entryArgument.properties.name.minLength,
  1,
  "debug launch arg names must carry Marrow's non-empty constraint",
);
assert.equal(
  launchDefs.entryArgument.properties.name.pattern,
  "\\S",
  "debug launch arg names must reject whitespace-only strings",
);
assert.deepEqual(
  launchDefs.entryArgument.properties.value.$ref,
  "#/$defs/entryValue",
  "debug launch arg values must use the recursive Marrow value schema",
);
const launchArgValueVariants = launchDefs.entryValue.oneOf;
assert.ok(
  Array.isArray(launchArgValueVariants),
  "debug launch arg value schema must expose typed Marrow variants",
);
const scalarVariants = launchDefs.entryScalar.oneOf;
const intArgValue = scalarVariants.find(
  (variant) => variant.properties?.kind?.const === "int",
);
assert.deepEqual(
  intArgValue?.properties?.value,
  { type: "string" },
  "debug launch int args must use Marrow's exact string form",
);
const bytesArgValue = scalarVariants.find(
  (variant) => variant.properties?.kind?.const === "bytes",
);
assert.deepEqual(
  bytesArgValue?.properties?.value,
  { type: "string", pattern: "^([0-9a-f]{2})*$" },
  "debug launch bytes args must use Marrow's lowercase hex string form",
);
const sequenceArgValue = launchArgValueVariants.find(
  (variant) => variant.properties?.kind?.const === "sequence",
);
assert.deepEqual(
  sequenceArgValue?.properties?.value?.items,
  { $ref: "#/$defs/entryValue" },
  "debug launch sequence args must recurse into the typed Marrow value schema",
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
  "defaultEntry",
  /defaultEntry/i,
);
const debugSnippetBody = marrowDebugger.configurationSnippets[0].body;
assert.equal(
  Object.hasOwn(debugSnippetBody, "entry"),
  false,
  "default debug snippet should not force an explicit entry",
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

const readmeF5 = markdownBullet(readme, "README.md", "Debugging (F5)");
const readmeSourceIntelligence = markdownBullet(readme, "README.md", "Source intelligence");
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "fact-backed completion framing",
  /completion are backed by Marrow facts over the current analysis/i,
);
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "catalog navigation ready contract",
  /Catalog-backed\s+go-to\s+definition\s+and\s+find\s+references[\s\S]*Marrow\s+navigation\s+facts/i,
);
assertDoesNotMention(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "store resource leaf graduation",
  /store\s+catalog\s+leaves/i,
);
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "editor aid caveat",
  /Hover\s+and\s+remaining\s+navigation\s+helpers\s+stay\s+editor\s+aids/i,
);
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "source-only rename fact framing",
  /Source-only\s+prepare\s+rename\s+and\s+editor\s+rename\s+edits\s+are\s+backed\s+by\s+Marrow\s+rename\s+facts/i,
);
assertMentions(
  readmeSourceIntelligence,
  "README.md source intelligence bullet",
  "remaining rename application caveat",
  /saved-data-backed\s+rename\s+application[\s\S]*remaining\s+hover\/navigation\s+facts stay owned by Marrow/i,
);
assertMentions(
  readme,
  "README.md",
  "rename source-only contract",
  /editor\s+rename\s+for\s+Marrow-admitted\s+source-only\s+bindings/i,
);
assertMentions(
  readme,
  "README.md",
  "rename editor application caveat",
  /evolve\s+rename\s+intent\s+has\s+a\s+canonical\s+editor\s+application\s+contract/i,
);
assertDoesNotMention(
  `${readme}\n${changelog}`,
  "VS Code copy",
  "stale catalog-evolution rename blocker",
  /until\s+Marrow\s+exposes\s+catalog-backed\s+evolution\s+facts/i,
);
assertDoesNotMention(
  readme,
  "README.md",
  "retired advisory live-data framing",
  /marrow\.liveData[\s\S]*advisory/i,
);
assertDoesNotMention(
  readme,
  "README.md",
  "retired integrity live-data framing",
  /marrow\.liveData[\s\S]*integrity/i,
);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "typed debug args framing",
  /typed\s+`args`/i,
);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "exact scalar debug args framing",
  /exact\s+string\s+forms/i,
);
assertMentions(
  readmeF5,
  "README.md F5 bullet",
  "explicit entry framing",
  /set\s+`entry`/i,
);
assertDoesNotMention(
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

const contributedCommands = packageJson.contributes.commands.map((command) => command.command);
assert.equal(
  contributedCommands.includes("marrow.checkDataIntegrity"),
  false,
  "VS Code must not contribute the retired advisory data-integrity command",
);
assert.equal(
  Object.hasOwn(packageJson.scripts, "check:dataIntegrity"),
  false,
  "VS Code checks must not keep a retired data-integrity script",
);
assertDoesNotMention(
  files.get("src/extension.ts"),
  "src/extension.ts",
  "retired data-integrity command wiring",
  /dataIntegrity|DataIntegrity|checkDataIntegrity|marrow\.checkDataIntegrity/,
);
