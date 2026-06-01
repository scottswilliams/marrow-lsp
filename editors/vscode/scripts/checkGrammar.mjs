import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const grammarPath = join(root, "syntaxes", "marrow.tmLanguage.json");
const packagePath = join(root, "package.json");
const grammar = JSON.parse(readFileSync(grammarPath, "utf8"));
const packageManifest = JSON.parse(readFileSync(packagePath, "utf8"));
const repo = grammar.repository;

function flattenPatterns(patterns, result = []) {
  for (const pattern of patterns ?? []) {
    result.push(pattern);
    flattenPatterns(pattern.patterns, result);
  }
  return result;
}

function assertPatternMatches(pattern, sample, description) {
  assert.ok(pattern, `${description}: pattern exists`);
  assert.ok(pattern.match, `${description}: pattern has a match regex`);
  assert.doesNotThrow(() => new RegExp(pattern.match), `${description}: regex compiles in JavaScript`);

  const regex = new RegExp(pattern.match);
  assert.ok(regex.test(sample), `${description}: regex matches ${JSON.stringify(sample)}`);
}

function captureNames(pattern) {
  return new Set(Object.values(pattern.captures ?? {}).map((capture) => capture.name));
}

function keywordPattern(scope) {
  return flattenPatterns(repo.keywords?.patterns).find((pattern) => pattern.name === scope);
}

function assertScopeMapping(scopes, token, expectedScopes) {
  assert.ok(scopes[token], `${token} semantic token has fallback scopes`);
  assert.deepEqual(scopes[token], expectedScopes, `${token} semantic token fallback scopes`);
}

const declarationKeyword = keywordPattern("keyword.declaration.marrow");
assertPatternMatches(declarationKeyword, "enum", "enum declaration keyword");

const topIncludes = grammar.patterns.map((pattern) => pattern.include);
const sourceIdentityAnnotationRule = "stable-id";
const sourceIdentityAnnotationHooks = [
  ...topIncludes.filter((include) => include === `#${sourceIdentityAnnotationRule}`),
  ...Object.keys(repo).filter((key) => key === sourceIdentityAnnotationRule),
];
assert.deepEqual(sourceIdentityAnnotationHooks, [], "source identity annotation grammar hooks are absent");
assert.ok(
  topIncludes.indexOf("#comments") < topIncludes.indexOf("#declaration-names") &&
    topIncludes.indexOf("#strings") < topIncludes.indexOf("#declaration-names"),
  "comments and strings stay before declaration-name fallback patterns",
);

const declarationPatterns = flattenPatterns(repo["declaration-names"]?.patterns);
const functionDeclaration = declarationPatterns.find((pattern) => pattern.match?.includes("fn"));
assertPatternMatches(functionDeclaration, "fn hydrate", "function declaration name");
assert.ok(captureNames(functionDeclaration).has("entity.name.function.marrow"), "fn name has function scope");

const resourceDeclaration = declarationPatterns.find((pattern) => pattern.match?.includes("resource"));
assertPatternMatches(resourceDeclaration, "resource Account", "resource declaration name");
assert.ok(
  captureNames(resourceDeclaration).has("entity.name.type.resource.marrow"),
  "resource name has resource type scope",
);

const enumDeclaration = declarationPatterns.find((pattern) => pattern.match?.includes("enum"));
assertPatternMatches(enumDeclaration, "enum Status", "enum declaration name");
assert.ok(captureNames(enumDeclaration).has("entity.name.type.enum.marrow"), "enum name has enum type scope");

const modulePath = repo["module-path"];
assert.ok(modulePath?.begin, "module path pattern has a begin regex");
assert.ok(modulePath.beginCaptures?.["1"]?.name === "keyword.declaration.marrow", "module keyword is scoped");
assert.ok(
  flattenPatterns(modulePath.patterns).some((pattern) => pattern.name === "entity.name.namespace.module.marrow"),
  "module path segments have module namespace scope",
);

const usePath = repo["use-path"];
assert.ok(usePath?.begin, "use path pattern has a begin regex");
assert.ok(usePath.beginCaptures?.["1"]?.name === "keyword.declaration.marrow", "use keyword is scoped");
assert.ok(
  flattenPatterns(usePath.patterns).some((pattern) => pattern.name === "entity.name.namespace.marrow"),
  "use path segments have namespace scope",
);

const configurationDefaults = packageManifest.contributes?.configurationDefaults;
assert.equal(
  configurationDefaults?.["[marrow]"]?.["editor.semanticHighlighting.enabled"],
  true,
  "Marrow enables semantic highlighting by default",
);

const marrowSemanticScopes = packageManifest.contributes?.semanticTokenScopes?.find(
  (entry) => entry.language === "marrow",
)?.scopes;
assert.ok(marrowSemanticScopes, "Marrow contributes semantic token fallback scopes");

assertScopeMapping(marrowSemanticScopes, "namespace", ["entity.name.namespace.marrow"]);
assertScopeMapping(marrowSemanticScopes, "namespace.defaultLibrary", [
  "support.namespace.builtin.marrow",
  "entity.name.namespace.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "function", ["entity.name.function.marrow"]);
assertScopeMapping(marrowSemanticScopes, "function.defaultLibrary", [
  "support.function.builtin.marrow",
  "entity.name.function.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "struct", [
  "entity.name.type.struct.marrow",
  "entity.name.type.resource.marrow",
  "entity.name.type.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "enum", [
  "entity.name.type.enum.marrow",
  "entity.name.type.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "enumMember", [
  "constant.other.enum.marrow",
  "variable.other.enummember.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "property", ["variable.other.property.marrow"]);
assertScopeMapping(marrowSemanticScopes, "parameter", ["variable.parameter.marrow"]);
assertScopeMapping(marrowSemanticScopes, "keyword", [
  "keyword.control.marrow",
  "keyword.declaration.marrow",
  "storage.modifier.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "booleanLiteral", ["constant.language.boolean.marrow"]);
assertScopeMapping(marrowSemanticScopes, "variable", ["variable.other.marrow"]);
assertScopeMapping(marrowSemanticScopes, "string", [
  "string.quoted.double.marrow",
  "string.quoted.double.bytes.marrow",
  "string.quoted.double.interpolated.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "number", [
  "constant.numeric.integer.marrow",
  "constant.numeric.decimal.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "comment", [
  "comment.block.documentation.marrow",
  "comment.line.semicolon.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "type", [
  "support.type.primitive.marrow",
  "support.type.error.marrow",
  "entity.name.type.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "type.defaultLibrary", [
  "support.type.builtin.marrow",
  "entity.name.type.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "operator", [
  "keyword.operator.marrow",
  "keyword.operator.logical.marrow",
  "keyword.operator.optional.marrow",
  "keyword.operator.range.marrow",
  "keyword.operator.comparison.marrow",
  "keyword.operator.assignment.marrow",
  "keyword.operator.arithmetic.marrow",
  "keyword.operator.concat.marrow",
]);
assertScopeMapping(marrowSemanticScopes, "savedRoot", [
  "variable.other.saved.marrow",
  "variable.other.marrow",
]);
