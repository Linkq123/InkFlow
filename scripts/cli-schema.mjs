import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const mode = process.argv[2];
if (mode !== "write" && mode !== "check") {
  console.error("Usage: node scripts/cli-schema.mjs write|check");
  process.exit(2);
}

const projectRoot = resolve(import.meta.dirname, "..");
const schemaPath = resolve(projectRoot, "docs", "inkflow-cli.schema.json");
const result = spawnSync(
  "cargo",
  [
    "run",
    "--quiet",
    "--manifest-path",
    resolve(projectRoot, "src-tauri", "Cargo.toml"),
    "--bin",
    "inkflow-cli",
    "--no-default-features",
    "--features",
    "cli",
    "--",
    "--format",
    "json",
    "schema",
  ],
  { cwd: projectRoot, encoding: "utf8" },
);
if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 3);
}
const envelope = JSON.parse(result.stdout);
if (envelope.apiVersion !== "inkflow.cli/v1" || envelope.ok !== true) {
  throw new Error("inkflow-cli returned an unexpected schema envelope.");
}
validateLocalReferences(envelope.data);
validateEditLineNumbers(envelope.data);
const generated = `${JSON.stringify(envelope.data, null, 2)}\n`;
if (mode === "write") {
  writeFileSync(schemaPath, generated, "utf8");
  console.log(schemaPath);
} else {
  let existing = "";
  try {
    existing = readFileSync(schemaPath, "utf8").replaceAll("\r\n", "\n");
  } catch {
    console.error(`Missing generated CLI schema: ${schemaPath}`);
    process.exit(1);
  }
  if (existing !== generated) {
    console.error("Generated CLI JSON Schema has drifted. Run 'pnpm cli:schema'.");
    process.exit(1);
  }
}

function validateLocalReferences(schema) {
  const missing = new Set();
  const visit = (value) => {
    if (!value || typeof value !== "object") return;
    if (typeof value.$ref === "string" && value.$ref.startsWith("#/")) {
      let current = schema;
      for (const encodedPart of value.$ref.slice(2).split("/")) {
        const part = encodedPart.replaceAll("~1", "/").replaceAll("~0", "~");
        current = current?.[part];
      }
      if (current === undefined) missing.add(value.$ref);
    }
    for (const child of Object.values(value)) visit(child);
  };
  visit(schema);
  if (missing.size > 0) {
    throw new Error(
      `Generated CLI JSON Schema contains unresolved local references: ${[
        ...missing,
      ].join(", ")}`,
    );
  }
}

function validateEditLineNumbers(schema) {
  const operations = schema?.$defs?.DocumentEditOperation?.oneOf;
  if (!Array.isArray(operations)) {
    throw new Error("Generated CLI JSON Schema is missing document edit operations.");
  }
  for (const type of ["block", "toggleTask", "table"]) {
    const operation = operations.find(
      (candidate) => candidate?.properties?.type?.const === type,
    );
    if (operation?.properties?.line?.minimum !== 1) {
      throw new Error(`CLI edit operation ${type} must use 1-based line numbers.`);
    }
  }
}
