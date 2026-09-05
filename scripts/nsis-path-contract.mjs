import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const hooksPath = resolve(import.meta.dirname, "..", "src-tauri", "windows", "hooks.nsh");
const hooks = readFileSync(hooksPath, "utf8");

for (const required of [
  "PathValueExisted",
  "INKFLOW_PATH_EXISTED",
  "$$next=$$path+$$entry+$\\';$\\'",
  "$$key.DeleteValue($\\'Path$\\',$$false)",
  "$$indexes.Count -ne 1",
  "$0 == 3",
]) {
  assert.ok(hooks.includes(required), `NSIS PATH hook is missing: ${required}`);
}

const entry = "C:\\Program Files\\InkFlow";
const cases = [
  { name: "missing value", state: { exists: false, value: "" } },
  { name: "empty value", state: { exists: true, value: "" } },
  { name: "normal value", state: { exists: true, value: "C:\\Windows" } },
  { name: "trailing separator", state: { exists: true, value: "C:\\Windows;" } },
  { name: "multiple trailing separators", state: { exists: true, value: "C:\\Windows;;" } },
];

for (const testCase of cases) {
  const installed = installPath(testCase.state, entry);
  const result = uninstallPath(installed, entry);
  assert.equal(result.ambiguous, false, `${testCase.name} must have one owned entry`);
  assert.deepEqual(result.state, testCase.state, `${testCase.name} must round-trip`);
}

const normallyInstalled = installPath({ exists: true, value: "C:\\Windows" }, entry);
for (const duplicate of [
  { name: "duplicate appended after install", value: `${normallyInstalled.value};${entry}` },
  { name: "duplicate prepended after install", value: `${entry};${normallyInstalled.value}` },
  {
    name: "case-insensitive duplicate appended after install",
    value: `${normallyInstalled.value};${entry.toLowerCase()}`,
  },
]) {
  const changed = { ...normallyInstalled, value: duplicate.value };
  const result = uninstallPath(changed, entry);
  assert.equal(result.ambiguous, true, `${duplicate.name} must be treated as ambiguous`);
  assert.deepEqual(result.state, changed, `${duplicate.name} must preserve PATH exactly`);
}

function installPath(original, pathEntry) {
  const path = original.value;
  assert.ok(!path.split(";").includes(pathEntry), "test input already contains InkFlow");
  const value = !path
    ? pathEntry
    : path.endsWith(";")
      ? `${path}${pathEntry};`
      : `${path};${pathEntry}`;
  return { exists: true, value, originalExisted: original.exists };
}

function uninstallPath(installed, pathEntry) {
  const entries = installed.value.split(";");
  const indexes = entries
    .map((value, index) => equalPath(value, pathEntry) ? index : -1)
    .filter((index) => index >= 0);
  assert.notEqual(indexes.length, 0, "installed PATH entry must exist");
  if (indexes.length !== 1) {
    return { ambiguous: true, state: installed };
  }
  const [index] = indexes;
  entries.splice(index, 1);
  if (!installed.originalExisted && entries.length === 0) {
    return { ambiguous: false, state: { exists: false, value: "" } };
  }
  return { ambiguous: false, state: { exists: true, value: entries.join(";") } };
}

function equalPath(left, right) {
  return left.localeCompare(right, "en-US", { sensitivity: "accent" }) === 0;
}
