import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const iconDirectory = path.join(projectRoot, "src-tauri", "icons");
const tauriConfigPath = path.join(projectRoot, "src-tauri", "tauri.conf.json");
const appCssPath = path.join(projectRoot, "src", "app.css");
const brandLogoPath = path.join(projectRoot, "logo.png");
const readmePath = path.join(projectRoot, "README.md");
const portableScriptPath = path.join(projectRoot, "scripts", "package-portable.ps1");

const pngContracts = new Map([
  ["32x32.png", 32],
  ["64x64.png", 64],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 512],
  ["StoreLogo.png", 50],
  ...[30, 44, 71, 89, 107, 142, 150, 284, 310].map((size) => [`Square${size}x${size}Logo.png`, size]),
]);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function pngSize(buffer, name) {
  assert(buffer.length >= 24 && buffer.toString("ascii", 1, 4) === "PNG", `${name} is not a PNG file`);
  return { width: buffer.readUInt32BE(16), height: buffer.readUInt32BE(20), colorType: buffer[25] };
}

function icoSizes(buffer) {
  assert(buffer.length >= 6 && buffer.readUInt16LE(0) === 0 && buffer.readUInt16LE(2) === 1, "icon.ico is not a valid ICO file");
  const count = buffer.readUInt16LE(4);
  assert(buffer.length >= 6 + count * 16, "icon.ico has a truncated directory");
  return Array.from({ length: count }, (_, index) => {
    const offset = 6 + index * 16;
    return [buffer[offset] || 256, buffer[offset + 1] || 256];
  });
}

const brandLogo = pngSize(await readFile(brandLogoPath), "logo.png");
assert(brandLogo.width === 1407 && brandLogo.height === 768, "logo.png must remain the approved optimized 1407x768 brand artwork");
assert(brandLogo.colorType === 6, "logo.png must remain an RGBA PNG");

const readme = await readFile(readmePath, "utf8");
assert(/<img\s+src=["']logo\.png["']\s+alt=["']InkFlow["']/i.test(readme), "README must display logo.png directly");
const portableScript = await readFile(portableScriptPath, "utf8");
assert(
  /if\s*\(\$referencesBrandLogo\)\s*\{[\s\S]*Copy-Item\s+-LiteralPath\s+\$logoPath\s+-Destination\s+\$staging[\s\S]*\}/i.test(portableScript),
  "The portable package must include logo.png when its README references the asset",
);

for (const [name, expectedSize] of pngContracts) {
  const size = pngSize(await readFile(path.join(iconDirectory, name)), name);
  assert(size.width === expectedSize && size.height === expectedSize, `${name} must be ${expectedSize}x${expectedSize}`);
  assert(size.colorType === 6, `${name} must preserve RGBA transparency`);
}

const ico = await readFile(path.join(iconDirectory, "icon.ico"));
const sizes = icoSizes(ico);
for (const expected of [16, 24, 32, 48, 64, 128, 256]) {
  assert(sizes.some(([width, height]) => width === expected && height === expected), `icon.ico is missing its ${expected}x${expected} frame`);
}

const tauriConfig = JSON.parse(await readFile(tauriConfigPath, "utf8"));
const nsisConfig = tauriConfig?.bundle?.windows?.nsis;
assert(nsisConfig?.installerIcon === "icons/icon.ico", "The NSIS installer must use icons/icon.ico");
assert(nsisConfig?.uninstallerIcon === "icons/icon.ico", "The NSIS uninstaller must use icons/icon.ico");

const appCss = await readFile(appCssPath, "utf8");
assert(/\.workspace-grid\s*>\s*\.file-sidebar\s*\{[^}]*grid-column:\s*1\b/s.test(appCss), "The file sidebar must remain in workspace grid column 1");
assert(/\.writing-area\s*\{[^}]*grid-column:\s*2\b/s.test(appCss), "The writing area must remain in workspace grid column 2");
assert(/\.workspace-grid\s*>\s*\.outline-sidebar\s*\{[^}]*grid-column:\s*3\b/s.test(appCss), "The outline sidebar must remain in workspace grid column 3");

console.log("InkFlow brand logo, generated PNGs, and Windows ICO satisfy their contracts.");
