import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { finalizeRelease, validateRelease } from "../../scripts/release-metadata.mjs";

const roots: string[] = [];

async function fixture(overrides: Partial<Record<"package" | "tauri" | "cargo" | "lock" | "notes", string>> = {}) {
  const root = await mkdtemp(join(tmpdir(), "inkflow-release-"));
  roots.push(root);
  await mkdir(join(root, "src-tauri"), { recursive: true });
  await mkdir(join(root, "docs", "releases"), { recursive: true });
  await writeFile(join(root, "package.json"), JSON.stringify({ version: overrides.package ?? "1.2.3" }));
  await writeFile(join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ version: overrides.tauri ?? "1.2.3" }));
  await writeFile(join(root, "src-tauri", "Cargo.toml"), `[package]\nname = "inkflow"\nversion = "${overrides.cargo ?? "1.2.3"}"\n\n[dependencies]\n`);
  await writeFile(join(root, "src-tauri", "Cargo.lock"), `[[package]]\nname = "inkflow"\nversion = "${overrides.lock ?? "1.2.3"}"\ndependencies = []\n`);
  if (overrides.notes !== "missing") {
    await writeFile(join(root, "docs", "releases", "v1.2.3.md"), overrides.notes ?? "# InkFlow 1.2.3\n\n## 更新\n\n- Update.\n\n## 修复\n\n- Fix.\n");
  }
  return root;
}

afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("release metadata", () => {
  it("accepts a stable tag when all version sources and notes agree", async () => {
    const root = await fixture();
    const result = await validateRelease({ projectRoot: root, tag: "v1.2.3" });
    expect(result.version).toBe("1.2.3");
  });

  it("rejects tags outside the vX.Y.Z contract", async () => {
    const root = await fixture();
    await expect(validateRelease({ projectRoot: root, tag: "release-1.2.3" }))
      .rejects.toThrow("must match vX.Y.Z");
  });

  it.each(["package", "tauri", "cargo", "lock"] as const)(
    "rejects a mismatched %s version source",
    async (source) => {
      const root = await fixture({ [source]: "1.2.2" });
      await expect(validateRelease({ projectRoot: root, tag: "v1.2.3" }))
        .rejects.toThrow("expected '1.2.3'");
    },
  );

  it("rejects missing release notes", async () => {
    const root = await fixture({ notes: "missing" });
    await expect(validateRelease({ projectRoot: root, tag: "v1.2.3" }))
      .rejects.toThrow("Release notes are missing");
  });

  it("can validate tagged source with release notes supplied by current tooling", async () => {
    const sourceRoot = await fixture({ notes: "missing" });
    const toolingRoot = await fixture();
    const result = await validateRelease({
      projectRoot: sourceRoot,
      releaseNotesRoot: toolingRoot,
      tag: "v1.2.3",
    });
    expect(result.notePath).toBe(join(toolingRoot, "docs", "releases", "v1.2.3.md"));
  });

  it.each(["## 修复\n\n- Fix.", "## 更新\n\n- Update."])(
    "requires both update and fix sections",
    async (notes) => {
      const root = await fixture({ notes });
      await expect(validateRelease({ projectRoot: root, tag: "v1.2.3" })).rejects.toThrow();
    },
  );

  it("rejects finalization when an asset is missing", async () => {
    const root = await fixture();
    await writeFile(join(root, "installer.exe"), "installer");
    await expect(finalizeRelease({
      projectRoot: root,
      tag: "v1.2.3",
      installerPath: "installer.exe",
      portablePath: "missing.zip",
      repository: "Linkq123/InkFlow",
    })).rejects.toThrow();
  });

  it("writes release notes and machine-readable SHA-256 values", async () => {
    const root = await fixture();
    await writeFile(join(root, "installer.exe"), "installer");
    await writeFile(join(root, "portable.zip"), "portable");
    const result = await finalizeRelease({
      projectRoot: root,
      tag: "v1.2.3",
      installerPath: "installer.exe",
      portablePath: "portable.zip",
      repository: "Linkq123/InkFlow",
      previousTag: "v1.2.2",
    });
    const checksums = await readFile(result.checksumPath, "utf8");
    const expected = createHash("sha256").update("installer").digest("hex");
    expect(checksums).toContain(`${expected} *installer.exe`);
    expect(await readFile(result.finalNotesPath, "utf8")).toContain("compare/v1.2.2...v1.2.3");
  });
});
