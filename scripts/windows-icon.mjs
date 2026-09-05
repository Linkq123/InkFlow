import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

function decodeSize(value) {
  return value === 0 ? 256 : value;
}

function readEntries(buffer) {
  if (buffer.length < 6 || buffer.readUInt16LE(0) !== 0 || buffer.readUInt16LE(2) !== 1) {
    throw new Error("Not a Windows ICO file");
  }
  const count = buffer.readUInt16LE(4);
  if (buffer.length < 6 + count * 16) throw new Error("Truncated ICO directory");
  return Array.from({ length: count }, (_, index) => {
    const offset = 6 + index * 16;
    const byteLength = buffer.readUInt32LE(offset + 8);
    const dataOffset = buffer.readUInt32LE(offset + 12);
    if (dataOffset + byteLength > buffer.length) throw new Error("Truncated ICO image data");
    return {
      width: decodeSize(buffer[offset]),
      height: decodeSize(buffer[offset + 1]),
      colorCount: buffer[offset + 2],
      reserved: buffer[offset + 3],
      planes: buffer.readUInt16LE(offset + 4),
      bitCount: buffer.readUInt16LE(offset + 6),
      data: buffer.subarray(dataOffset, dataOffset + byteLength),
    };
  });
}

function writeEntries(entries) {
  const headerLength = 6 + entries.length * 16;
  let dataOffset = headerLength;
  const header = Buffer.alloc(headerLength);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);
  entries.forEach((entry, index) => {
    const offset = 6 + index * 16;
    header[offset] = entry.width === 256 ? 0 : entry.width;
    header[offset + 1] = entry.height === 256 ? 0 : entry.height;
    header[offset + 2] = entry.colorCount;
    header[offset + 3] = entry.reserved;
    header.writeUInt16LE(entry.planes, offset + 4);
    header.writeUInt16LE(entry.bitCount, offset + 6);
    header.writeUInt32LE(entry.data.length, offset + 8);
    header.writeUInt32LE(dataOffset, offset + 12);
    dataOffset += entry.data.length;
  });
  return Buffer.concat([header, ...entries.map((entry) => entry.data)]);
}

function assertPngSize(buffer, expected) {
  if (buffer.length < 24 || buffer.toString("ascii", 1, 4) !== "PNG") {
    throw new Error("The additional ICO frame is not a PNG");
  }
  const width = buffer.readUInt32BE(16);
  const height = buffer.readUInt32BE(20);
  if (width !== expected || height !== expected) {
    throw new Error(`Expected a ${expected}x${expected} PNG, got ${width}x${height}`);
  }
}

export async function addPngFrame(icoPath, pngPath, size) {
  const [ico, png] = await Promise.all([readFile(icoPath), readFile(pngPath)]);
  assertPngSize(png, size);
  const entries = readEntries(ico).filter((entry) => entry.width !== size || entry.height !== size);
  entries.push({ width: size, height: size, colorCount: 0, reserved: 0, planes: 1, bitCount: 32, data: png });
  entries.sort((left, right) => left.width - right.width);
  await writeFile(icoPath, writeEntries(entries));
}

if (process.argv[1] && path.resolve(fileURLToPath(import.meta.url)) === path.resolve(process.argv[1])) {
  const [command, icoPath, pngPath, rawSize] = process.argv.slice(2);
  if (command !== "add-frame" || !icoPath || !pngPath || !rawSize) {
    console.error("Usage: node scripts/windows-icon.mjs add-frame ICON.ico FRAME.png SIZE");
    process.exitCode = 2;
  } else {
    await addPngFrame(icoPath, pngPath, Number(rawSize));
  }
}
