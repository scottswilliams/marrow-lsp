import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const grammarPath = join(root, "syntaxes", "marrow.tmLanguage.json");
const grammar = JSON.parse(readFileSync(grammarPath, "utf8"));
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

const declarationKeyword = keywordPattern("keyword.declaration.marrow");
assertPatternMatches(declarationKeyword, "enum", "enum declaration keyword");

const topIncludes = grammar.patterns.map((pattern) => pattern.include);
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
