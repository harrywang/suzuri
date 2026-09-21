import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
const stringLiteral = /"(?:[^"\\]|\\.)*"/;

function catalog(path) {
  const source = read(path);
  const keys = [...source.matchAll(/^\s*("(?:[^"\\]|\\.)*")\s*:/gm)].map((m) => JSON.parse(m[1]));
  assert.equal(new Set(keys).size, keys.length, `Duplicate keys in ${path}`);
  return JSON.parse(source);
}

const english = catalog("assets/locales/en.json");
const chinese = catalog("assets/locales/zh-CN.json");
assert.deepEqual(Object.keys(english).sort(), Object.keys(chinese).sort());
const placeholders = (text) => [...text.matchAll(/\{[^{}]*\}/g)].map((m) => m[0]).sort();
for (const key of Object.keys(english)) {
  assert.ok(english[key].trim() && chinese[key].trim(), `Empty translation: ${key}`);
  assert.deepEqual(placeholders(english[key]), placeholders(chinese[key]), `Placeholders: ${key}`);
}

const mappings = new Map();
const helper = read("crates/settings_ui/src/localization.rs");
for (const match of helper.matchAll(/^\s*("(?:[^"\\]|\\.)*")\s*=>\s*(?:\{\s*)?"([^"]+)"/gm)) {
  const source = JSON.parse(match[1]);
  assert.ok(!mappings.has(source), `Duplicate source mapping: ${source}`);
  assert.ok(match[2] in english, `Missing resource: ${match[2]}`);
  mappings.set(source, match[2]);
}

// This checks fixed row titles/descriptions, not dynamic provider views or visual layout.
const pages = ["crates/settings_ui/src/page_data.rs", ...readdirSync(new URL("crates/settings_ui/src/pages/", root))
  .filter((name) => name.endsWith(".rs"))
  .map((name) => `crates/settings_ui/src/pages/${name}`)];
let checked = 0;
for (const path of pages) {
  const source = read(path).split("#[cfg(test)]")[0];
  const expression = new RegExp(`(?:title|description):\\s*(${stringLiteral.source})`, "g");
  for (const match of source.matchAll(expression)) {
    const value = JSON.parse(match[1]);
    if (!value) continue;
    assert.ok(mappings.has(value), `Untranslated ${path}: ${value}`);
    checked++;
  }
}
console.log(`PASS: ${Object.keys(english).length} bilingual resources; ${checked} fixed settings titles/descriptions; no duplicate mappings or placeholder differences.`);
