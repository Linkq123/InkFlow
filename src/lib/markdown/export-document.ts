import { createMermaidResourceScope } from "./mermaid-resources";
import { renderForExportInWorker } from "./render-service";
import { renderMermaid } from "./mermaid-service";
import {
  type ImageSrcsetCandidate,
  blockRemoteImageRequests,
  hasRemoteMermaidImageReference,
  hasRetainedResponsiveImageSource,
  isRemoteImageSource,
  parseImageSrcset,
  resolveLocalMermaidImageReferences,
  serializeImageSrcset,
} from "./resources";

export interface ExportDocumentOptions {
  allowRemoteImages: boolean;
  editorFont: string;
  loadResource?: (source: string) => Promise<string>;
}

/**
 * Produces the exact sanitized fragment used by desktop HTML/PDF export and
 * the hidden CLI renderer. No local file is loaded without the caller's
 * scoped loadResource callback, and remote images remain inert by default.
 */
export async function prepareExportDocument(
  markdown: string,
  options: ExportDocumentOptions,
): Promise<string> {
  const rawRendered = await renderForExportInWorker(markdown);
  const rendered = options.allowRemoteImages
    ? rawRendered
    : blockRemoteImageRequests(rawRendered);
  const documentNode = new DOMParser().parseFromString(
    `<main>${rendered}</main>`,
    "text/html",
  );
  const resourceCache = new Map<string, Promise<string>>();
  const loadLocalResource = options.loadResource
    ? (source: string) => {
        let pending = resourceCache.get(source);
        if (!pending) {
          pending = options.loadResource!(source);
          resourceCache.set(source, pending);
        }
        return pending;
      }
    : undefined;

  const mermaidBlocks = Array.from(
    documentNode.querySelectorAll<HTMLElement>("pre > code.language-mermaid"),
  );
  for (const code of mermaidBlocks) {
    const pre = code.parentElement;
    if (!pre) continue;
    if (
      !options.allowRemoteImages
      && await hasRemoteMermaidImageReference(code.textContent ?? "")
    ) {
      pre.classList.add("render-error");
      pre.setAttribute("data-error", "Remote Mermaid image blocked");
      continue;
    }
    const mermaidResources = createMermaidResourceScope(loadLocalResource);
    try {
      const source = await resolveLocalMermaidImageReferences(
        code.textContent ?? "",
        mermaidResources.load,
      );
      const result = await renderMermaid(
        source,
        {
          startOnLoad: false,
          securityLevel: "strict",
          theme: "neutral",
          fontFamily: options.editorFont,
        },
        "inkflow-export",
        undefined,
        options.allowRemoteImages,
      );
      const figure = documentNode.createElement("figure");
      figure.className = "mermaid-diagram";
      const svg = mermaidResources.restore(result.svg);
      figure.innerHTML = options.allowRemoteImages
        ? svg
        : blockRemoteImageRequests(svg);
      pre.replaceWith(figure);
    } catch {
      // Keep the original source block when Mermaid rejects its syntax.
    }
  }
  await embedImageResources(
    documentNode,
    documentNode,
    options.allowRemoteImages,
    loadLocalResource,
  );
  return documentNode.querySelector("main")?.innerHTML ?? rendered;
}

async function embedImageResources(
  root: ParentNode,
  documentNode: Document,
  allowRemoteImages: boolean,
  loadLocalResource?: (source: string) => Promise<string>,
): Promise<void> {
  if (loadLocalResource) {
    for (const element of Array.from(
      root.querySelectorAll<HTMLElement>("source[srcset], img[srcset]"),
    )) {
      const candidates = parseImageSrcset(element.getAttribute("srcset") ?? "");
      const embedded: ImageSrcsetCandidate[] = [];
      for (const candidate of candidates) {
        if (
          isRemoteImageSource(candidate.source) ||
          isEmbeddedImageSource(candidate.source)
        ) {
          embedded.push(candidate);
          continue;
        }
        try {
          embedded.push({
            ...candidate,
            source: await loadLocalResource(candidate.source),
          });
        } catch {
          // Drop an unavailable candidate so <picture> can use another source
          // or the already handled <img src> fallback.
        }
      }
      if (embedded.length > 0) {
        element.setAttribute("srcset", serializeImageSrcset(embedded));
      } else {
        element.removeAttribute("srcset");
      }
    }
  }

  for (const image of Array.from(
    root.querySelectorAll<HTMLImageElement>("img"),
  )) {
    const blockedSource = image.getAttribute("data-inkflow-remote-src");
    if (blockedSource) {
      if (hasRetainedResponsiveImageSource(image)) {
        image.classList.remove("remote-blocked", "resource-missing");
        continue;
      }
      image.replaceWith(
        documentNode.createTextNode(
          `[Remote image blocked: ${image.alt || blockedSource}]`,
        ),
      );
      continue;
    }
    const source = image.getAttribute("src") ?? "";
    if (isRemoteImageSource(source)) {
      if (!allowRemoteImages) {
        image.removeAttribute("src");
        if (hasRetainedResponsiveImageSource(image)) {
          image.classList.remove("remote-blocked", "resource-missing");
          continue;
        }
        image.replaceWith(
          documentNode.createTextNode(
            `[Remote image blocked: ${image.alt || source}]`,
          ),
        );
      }
      continue;
    }
    if (!source || !loadLocalResource || isEmbeddedImageSource(source)) continue;
    try {
      image.src = await loadLocalResource(source);
      image.classList.remove("remote-blocked", "resource-missing");
    } catch {
      if (hasRetainedResponsiveImageSource(image)) {
        image.removeAttribute("src");
        image.classList.remove("remote-blocked", "resource-missing");
        continue;
      }
      image.replaceWith(
        documentNode.createTextNode(`[Missing image: ${image.alt || source}]`),
      );
    }
  }

  for (const image of Array.from(
    root.querySelectorAll<SVGImageElement>("svg image"),
  )) {
    const blockedSource =
      image.getAttribute("data-inkflow-remote-href") ??
      image.getAttribute("data-inkflow-remote-xlink-href");
    if (blockedSource) {
      image.removeAttribute("href");
      image.removeAttribute("xlink:href");
      continue;
    }
    const source =
      image.getAttribute("href") ?? image.getAttribute("xlink:href") ?? "";
    if (isRemoteImageSource(source)) {
      if (!allowRemoteImages) {
        image.removeAttribute("href");
        image.removeAttribute("xlink:href");
      }
      continue;
    }
    if (
      !source ||
      source.startsWith("#") ||
      !loadLocalResource ||
      isEmbeddedImageSource(source)
    ) {
      continue;
    }
    try {
      const embedded = await loadLocalResource(source);
      if (image.hasAttribute("href")) image.setAttribute("href", embedded);
      if (image.hasAttribute("xlink:href")) {
        image.setAttribute("xlink:href", embedded);
      }
    } catch {
      image.removeAttribute("href");
      image.removeAttribute("xlink:href");
    }
  }
}

function isEmbeddedImageSource(source: string): boolean {
  return /^(?:data:|blob:)/i.test(source.trim());
}
