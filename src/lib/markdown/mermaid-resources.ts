interface MermaidResourceScope {
  load?: (source: string) => Promise<string>;
  restore: (svg: string) => string;
}

export function createMermaidResourceScope(
  loadLocalResource?: (source: string) => Promise<string>,
): MermaidResourceScope {
  const placeholders = new Map<string, string>();
  return {
    load: loadLocalResource
      ? async (source: string) => {
          const embedded = await loadLocalResource(source);
          const placeholder = await createEmbeddedImagePlaceholder(
            embedded,
            placeholders.size,
          );
          if (!placeholder) return embedded;
          placeholders.set(placeholder, embedded);
          return placeholder;
        }
      : undefined,
    restore: (svg: string) => {
      let restored = svg;
      for (const [placeholder, embedded] of placeholders) {
        restored = restored.split(placeholder).join(embedded);
      }
      return restored;
    },
  };
}

async function createEmbeddedImagePlaceholder(
  source: string,
  sequence: number,
): Promise<string | null> {
  const objectUrl = createEmbeddedImageObjectUrl(source);
  if (!objectUrl || typeof Image !== "function") return null;
  const image = new Image();
  try {
    image.src = objectUrl;
    await image.decode();
    const width = Math.max(1, image.naturalWidth);
    const height = Math.max(1, image.naturalHeight);
    if (!Number.isFinite(width) || !Number.isFinite(height)) return null;
    const svg = [
      '<svg xmlns="http://www.w3.org/2000/svg"',
      ` width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">`,
      `<desc>inkflow-resource-${sequence}</desc></svg>`,
    ].join("");
    return `data:image/svg+xml;base64,${btoa(svg)}`;
  } catch {
    return null;
  } finally {
    image.src = "";
    URL.revokeObjectURL(objectUrl);
  }
}

function createEmbeddedImageObjectUrl(source: string): string | null {
  const normalized = source.trim();
  if (!normalized.toLowerCase().startsWith("data:")) return null;
  if (
    typeof URL.createObjectURL !== "function"
    || typeof URL.revokeObjectURL !== "function"
  ) {
    return null;
  }

  const separator = normalized.indexOf(",");
  if (separator < 5) return null;
  const metadata = normalized.slice(5, separator);
  const payload = normalized.slice(separator + 1);
  const mimeType = metadata.split(";", 1)[0] || "application/octet-stream";
  try {
    const content: BlobPart = /(?:^|;)base64(?:;|$)/i.test(metadata)
      ? decodeBase64Bytes(payload)
      : decodeURIComponent(payload);
    return URL.createObjectURL(new Blob([content], { type: mimeType }));
  } catch {
    return null;
  }
}

function decodeBase64Bytes(source: string): Uint8Array<ArrayBuffer> {
  const decoded = atob(source);
  const bytes = new Uint8Array(decoded.length);
  for (let index = 0; index < decoded.length; index += 1) {
    bytes[index] = decoded.charCodeAt(index);
  }
  return bytes;
}
