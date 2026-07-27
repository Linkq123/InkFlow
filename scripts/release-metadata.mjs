import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { basename, dirname, isAbsolute, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const STABLE_TAG = /^v(\d+)\.(\d+)\.(\d+)$/;

/**
 * @typedef {{ projectRoot: string, tag: string, releaseNotesRoot?: string }} ValidateReleaseOptions
 * @typedef {{
 *   projectRoot: string,
 *   tag: string,
 *   installerPath: string,
 *   portablePath: string,
 *   repository: string,
 *   outputDirectory?: string,
 *   previousTag?: string | null,
 *   releaseNotesRoot?: string,
 * }} FinalizeReleaseOptions
 * @typedef {[number, number, number]} StableVersion
 */

/** @param {string} text */
function packageSection(text) {
  const match = /^\[package\]\s*$/m.exec(text);
  if (!match) throw new Error("Cargo.toml does not contain a [package] section.");
  const rest = text.slice(match.index + match[0].length);
  const nextSection = /^\[/m.exec(rest);
  return nextSection ? rest.slice(0, nextSection.index) : rest;
}

/**
 * @param {string} text
 * @param {string} source
 */
function quotedVersion(text, source) {
  const match = /^version\s*=\s*"([^"]+)"\s*$/m.exec(text);
  if (!match) throw new Error(`${source} does not contain a package version.`);
  return match[1];
}

/** @param {string} text */
function lockPackageVersion(text) {
  const block = text
    .split(/\r?\n(?=\[\[package\]\])/)
    .find((candidate) => /^name\s*=\s*"inkflow"\s*$/m.test(candidate));
  if (!block) throw new Error("Cargo.lock does not contain the inkflow package.");
  return quotedVersion(block, "Cargo.lock");
}

/** @param {string} projectRoot */
async function readVersions(projectRoot) {
  const [packageJson, tauriConfig, cargoToml, cargoLock] = await Promise.all([
    readFile(resolve(projectRoot, "package.json"), "utf8").then(JSON.parse),
    readFile(resolve(projectRoot, "src-tauri", "tauri.conf.json"), "utf8").then(JSON.parse),
    readFile(resolve(projectRoot, "src-tauri", "Cargo.toml"), "utf8"),
    readFile(resolve(projectRoot, "src-tauri", "Cargo.lock"), "utf8"),
  ]);
  return {
    "package.json": String(packageJson.version ?? ""),
    "tauri.conf.json": String(tauriConfig.version ?? ""),
    "Cargo.toml": quotedVersion(packageSection(cargoToml), "Cargo.toml"),
    "Cargo.lock": lockPackageVersion(cargoLock),
  };
}

/** @param {ValidateReleaseOptions} options */
export async function validateRelease({ projectRoot, tag, releaseNotesRoot = projectRoot }) {
  const match = STABLE_TAG.exec(tag);
  if (!match) throw new Error(`Release tag must match vX.Y.Z: '${tag}'.`);
  const version = `${match[1]}.${match[2]}.${match[3]}`;
  const versions = await readVersions(projectRoot);
  const errors = Object.entries(versions)
    .filter(([, value]) => value !== version)
    .map(([source, value]) => `${source} has version '${value}', expected '${version}'.`);
  const notePath = resolve(releaseNotesRoot, "docs", "releases", `${tag}.md`);
  let notes = "";
  try {
    notes = await readFile(notePath, "utf8");
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") {
      errors.push(`Release notes are missing: docs/releases/${tag}.md.`);
    } else {
      throw error;
    }
  }
  if (notes && !/^##[ \t]+更新[ \t]*$/m.test(notes)) {
    errors.push("Release notes must contain a '## 更新' section.");
  }
  if (notes && !/^##[ \t]+修复[ \t]*$/m.test(notes)) {
    errors.push("Release notes must contain a '## 修复' section.");
  }
  if (errors.length) throw new Error(errors.join("\n"));
  return { tag, version, versions, notePath, notes };
}

/**
 * @param {string} path
 * @returns {Promise<string>}
 */
export async function hashFile(path) {
  await stat(path);
  return await new Promise((resolveHash, reject) => {
    const hash = createHash("sha256");
    const stream = createReadStream(path);
    stream.on("error", reject);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("end", () => resolveHash(hash.digest("hex")));
  });
}

/**
 * @param {string} tag
 * @returns {StableVersion | null}
 */
function stableVersion(tag) {
  const match = STABLE_TAG.exec(tag);
  return match ? /** @type {StableVersion} */ (match.slice(1).map(Number)) : null;
}

/**
 * @param {StableVersion} left
 * @param {StableVersion} right
 */
function compareVersions(left, right) {
  for (let index = 0; index < 3; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return 0;
}

/**
 * @param {string} projectRoot
 * @param {string} currentTag
 * @returns {string | null}
 */
export function findPreviousTag(projectRoot, currentTag) {
  const current = stableVersion(currentTag);
  if (!current) return null;
  try {
    return execFileSync("git", ["tag", "--list", "v*"], {
      cwd: projectRoot,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    })
      .split(/\r?\n/)
      .flatMap((tag) => {
        const version = stableVersion(tag);
        return version ? [{ tag, version }] : [];
      })
      .filter((entry) => compareVersions(entry.version, current) < 0)
      .sort((left, right) => compareVersions(right.version, left.version))[0]?.tag ?? null;
  } catch {
    return null;
  }
}

/**
 * @param {string} projectRoot
 * @param {string} outputDirectory
 */
function outputPathInsideProject(projectRoot, outputDirectory) {
  const outputRoot = resolve(projectRoot, outputDirectory);
  const pathFromRoot = relative(projectRoot, outputRoot);
  if (pathFromRoot.startsWith("..") || isAbsolute(pathFromRoot)) {
    throw new Error("Release output directory must stay inside the project.");
  }
  return outputRoot;
}

/** @param {FinalizeReleaseOptions} options */
export async function finalizeRelease({
  projectRoot,
  tag,
  installerPath,
  portablePath,
  repository,
  outputDirectory = "release",
  previousTag,
  releaseNotesRoot = projectRoot,
}) {
  const metadata = await validateRelease({ projectRoot, tag, releaseNotesRoot });
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error(`Invalid GitHub repository name: '${repository}'.`);
  }
  const resolvedInstaller = resolve(projectRoot, installerPath);
  const resolvedPortable = resolve(projectRoot, portablePath);
  const [installerHash, portableHash] = await Promise.all([
    hashFile(resolvedInstaller),
    hashFile(resolvedPortable),
  ]);
  const outputRoot = outputPathInsideProject(projectRoot, outputDirectory);
  await mkdir(outputRoot, { recursive: true });
  const checksumPath = resolve(outputRoot, `InkFlow-${metadata.version}-SHA256SUMS.txt`);
  const finalNotesPath = resolve(outputRoot, `release-notes-${tag}-final.md`);
  const installerName = basename(resolvedInstaller);
  const portableName = basename(resolvedPortable);
  const checksums = [
    `${installerHash} *${installerName}`,
    `${portableHash} *${portableName}`,
  ].join("\n") + "\n";
  await writeFile(checksumPath, checksums, "utf8");

  const baseTag = previousTag === undefined ? findPreviousTag(projectRoot, tag) : previousTag;
  const comparison = baseTag
    ? `\n\n**完整变更对比**：https://github.com/${repository}/compare/${baseTag}...${tag}`
    : "";
  const finalNotes = `${metadata.notes.trim()}\n\n## 验证与构建\n\n- 完整 \`pnpm verify\` 已通过。\n- Windows x64 Tauri/NSIS 生产构建已通过。\n\n## 下载与安全提示\n\n- \`${installerName}\`  \n  SHA-256：\`${installerHash.toUpperCase()}\`\n- \`${portableName}\`  \n  SHA-256：\`${portableHash.toUpperCase()}\`\n- \`${basename(checksumPath)}\` 提供机器可读的同一组校验值。\n\n当前 Windows 二进制文件尚未数字签名，Microsoft Defender SmartScreen 可能显示安全提醒。${comparison}\n`;
  await writeFile(finalNotesPath, finalNotes, "utf8");
  return {
    ...metadata,
    installerPath: resolvedInstaller,
    portablePath: resolvedPortable,
    installerHash,
    portableHash,
    checksumPath,
    finalNotesPath,
    previousTag: baseTag,
  };
}

/**
 * @param {string[]} args
 * @returns {Record<string, string>}
 */
function parseOptions(args) {
  /** @type {Record<string, string>} */
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const name = args[index];
    if (name === "--") continue;
    if (!name.startsWith("--")) throw new Error(`Unexpected argument: '${name}'.`);
    const value = args[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for '${name}'.`);
    options[name.slice(2)] = value;
    index += 1;
  }
  return options;
}

/** @returns {Promise<void>} */
async function runCli() {
  const scriptDirectory = dirname(fileURLToPath(import.meta.url));
  const [command, ...args] = process.argv.slice(2);
  const options = parseOptions(args);
  const projectRoot = options["project-root"]
    ? resolve(process.cwd(), options["project-root"])
    : resolve(scriptDirectory, "..");
  const releaseNotesRoot = options["release-notes-root"]
    ? resolve(process.cwd(), options["release-notes-root"])
    : projectRoot;
  if (!options.tag) throw new Error("The --tag option is required.");
  if (command === "validate") {
    console.log(JSON.stringify(await validateRelease({
      projectRoot,
      tag: options.tag,
      releaseNotesRoot,
    })));
    return;
  }
  if (command === "finalize") {
    for (const option of ["installer", "portable", "repository"]) {
      if (!options[option]) throw new Error(`The --${option} option is required.`);
    }
    console.log(JSON.stringify(await finalizeRelease({
      projectRoot,
      tag: options.tag,
      installerPath: options.installer,
      portablePath: options.portable,
      repository: options.repository,
      outputDirectory: options["output-directory"] ?? "release",
      releaseNotesRoot,
    })));
    return;
  }
  throw new Error(`Unknown release metadata command: '${command ?? ""}'.`);
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : "";
if (invokedPath === import.meta.url) {
  runCli().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
