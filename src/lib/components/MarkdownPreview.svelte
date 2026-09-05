<script lang="ts">
  import { onMount, tick } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api, isDesktop } from "../api/client";
  import { renderInWorker } from "../markdown/render-service";
  import { renderMermaid } from "../markdown/mermaid-service";
  import {
    type ImageSrcsetCandidate,
    blockRemoteImageRequests,
    hasRemoteMermaidImageReference,
    hasUsableResponsiveImageSource,
    isRemoteImageSource,
    parseImageSrcset,
    resolveLocalMermaidImageReferences,
    serializeImageSrcset,
  } from "../markdown/resources";

  export let value: string;
  export let documentId: string;
  export let allowRemoteImages = false;
  export let pageWidth = 820;
  export let fontSize = 16;
  export let lineHeight = 1.75;
  export let editorFont = "Segoe UI, sans-serif";
  export let theme: "light" | "dark" = "light";

  let container: HTMLElement;
  let html = "";
  let error = "";
  let renderToken = 0;
  let responsiveMediaCleanups: Array<() => void> = [];

  onMount(() => {
    container.addEventListener("click", handleClick);
    return () => {
      renderToken += 1;
      clearResponsiveMediaListeners();
      container.removeEventListener("click", handleClick);
    };
  });

  $: void refresh(value, documentId, allowRemoteImages, theme, editorFont);

  async function refresh(
    markdown: string,
    id: string,
    remote: boolean,
    currentTheme: string,
    currentFont: string,
  ): Promise<void> {
    const token = ++renderToken;
    clearResponsiveMediaListeners();
    try {
      const rendered = await renderInWorker(markdown);
      if (token !== renderToken) return;
      const nextHtml = remote ? rendered : blockRemoteImageRequests(rendered);
      // Theme/font changes can produce identical sanitized HTML after Mermaid
      // has already replaced its source blocks. Clear that hydrated DOM once so
      // the source blocks are recreated and rendered with the new settings.
      if (html === nextHtml && container?.hasChildNodes()) {
        html = "";
        await tick();
        if (token !== renderToken) return;
      }
      html = nextHtml;
      error = "";
      await tick();
      if (token !== renderToken) return;
      await hydrateImages(id, remote, token);
      if (token !== renderToken) return;
      await hydrateMermaid(id, currentTheme, remote, currentFont, token);
    } catch (cause) {
      if (token !== renderToken) return;
      if (cause instanceof DOMException && cause.name === "AbortError") return;
      error = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function hydrateImages(id: string, remote: boolean, token: number): Promise<void> {
    if (!container || token !== renderToken) return;
    const resourceCache = new Map<string, Promise<string>>();
    const loadLocalResource = (source: string): Promise<string> => {
      let pending = resourceCache.get(source);
      if (!pending) {
        pending = api.loadResource(id, source);
        resourceCache.set(source, pending);
      }
      return pending;
    };
    const responsiveElements = Array.from(
      container.querySelectorAll<HTMLElement>("source[srcset], img[srcset]"),
    );
    await Promise.all(responsiveElements.map(async (element) => {
      const candidates = parseImageSrcset(element.getAttribute("srcset") ?? "");
      const hydrated = await Promise.all(candidates.map(async (
        candidate,
      ): Promise<ImageSrcsetCandidate | null> => {
        if (isRemoteImageSource(candidate.source)) {
          return remote ? candidate : null;
        }
        if (!isDesktop() || isEmbeddedImageSource(candidate.source)) return candidate;
        try {
          return {
            ...candidate,
            source: await loadLocalResource(candidate.source),
          };
        } catch {
          return null;
        }
      }));
      if (token !== renderToken) return;
      const available = hydrated.filter(
        (candidate): candidate is ImageSrcsetCandidate => candidate !== null,
      );
      if (available.length > 0) {
        element.setAttribute("srcset", serializeImageSrcset(available));
        if (element instanceof HTMLImageElement) {
          element.classList.remove("remote-blocked", "resource-missing");
        }
      } else {
        element.removeAttribute("srcset");
      }
    }));
    if (token !== renderToken) return;
    const images = Array.from(container.querySelectorAll<HTMLImageElement>("img"));
    await Promise.all(images.map(async (image) => {
      if (token !== renderToken) return;
      const blockedSource = image.getAttribute("data-inkflow-remote-src");
      if (blockedSource) {
        bindResponsiveFallbackState(
          image,
          "remote-blocked",
          "Remote image blocked",
          blockedSource,
        );
        return;
      }
      const source = image.getAttribute("src") ?? "";
      if (isRemoteImageSource(source)) {
        if (!remote) {
          image.removeAttribute("src");
          bindResponsiveFallbackState(
            image,
            "remote-blocked",
            "Remote image blocked",
            source,
          );
        }
        return;
      }
      if (!isDesktop() || isEmbeddedImageSource(source) || !source) return;
      try {
        const loadedSource = await loadLocalResource(source);
        if (token !== renderToken) return;
        image.src = loadedSource;
        image.classList.remove("remote-blocked", "resource-missing");
      } catch {
        if (token !== renderToken) return;
        image.removeAttribute("src");
        bindResponsiveFallbackState(
          image,
          "resource-missing",
          "Missing image",
          source,
        );
      }
    }));
  }

  function bindResponsiveFallbackState(
    image: HTMLImageElement,
    stateClass: "remote-blocked" | "resource-missing",
    message: string,
    fallbackLabel: string,
  ): void {
    const originalAlt = image.alt;
    const otherStateClass = stateClass === "remote-blocked"
      ? "resource-missing"
      : "remote-blocked";
    const update = () => {
      const responsiveSourceIsUsable = hasUsableResponsiveImageSource(image);
      image.classList.toggle(stateClass, !responsiveSourceIsUsable);
      image.classList.remove(otherStateClass);
      image.alt = responsiveSourceIsUsable
        ? originalAlt
        : `${message}: ${originalAlt || fallbackLabel}`;
    };

    update();
    watchResponsiveMediaChanges(image, update);
  }

  function watchResponsiveMediaChanges(
    image: HTMLImageElement,
    update: () => void,
  ): void {
    const picture = image.parentElement;
    if (
      picture?.tagName !== "PICTURE"
      || typeof window.matchMedia !== "function"
    ) {
      return;
    }

    const watchedQueries = new Set<string>();
    for (const source of Array.from(picture.children)) {
      if (!(source instanceof HTMLSourceElement)) continue;
      if (!source.getAttribute("srcset")?.trim()) continue;
      const media = source.getAttribute("media")?.trim();
      if (!media || watchedQueries.has(media)) continue;
      watchedQueries.add(media);
      let query: MediaQueryList;
      try {
        query = window.matchMedia(media);
      } catch {
        continue;
      }
      const listener = () => update();
      if (typeof query.addEventListener === "function") {
        query.addEventListener("change", listener);
        responsiveMediaCleanups.push(() =>
          query.removeEventListener("change", listener)
        );
      } else {
        query.addListener(listener);
        responsiveMediaCleanups.push(() => query.removeListener(listener));
      }
    }
  }

  function clearResponsiveMediaListeners(): void {
    for (const cleanup of responsiveMediaCleanups) cleanup();
    responsiveMediaCleanups = [];
  }

  function isEmbeddedImageSource(source: string): boolean {
    return /^(?:data:|blob:)/i.test(source.trim());
  }

  async function hydrateMermaid(
    id: string,
    currentTheme: string,
    remote: boolean,
    currentFont: string,
    token: number,
  ): Promise<void> {
    if (!container || token !== renderToken) return;
    const blocks = Array.from(container.querySelectorAll<HTMLElement>("pre > code.language-mermaid"));
    if (blocks.length === 0) return;
    const remoteImageFlags = remote
      ? blocks.map(() => false)
      : await Promise.all(blocks.map((block) =>
        hasRemoteMermaidImageReference(block.textContent ?? "")
      ));
    if (token !== renderToken) return;
    const renderableBlocks = blocks.filter((block, index) => {
      if (!remoteImageFlags[index]) return true;
      const pre = block.parentElement;
      pre?.classList.add("render-error");
      pre?.setAttribute("data-error", "Remote Mermaid image blocked");
      return false;
    });
    if (renderableBlocks.length === 0) return;
    for (const block of renderableBlocks) {
      if (token !== renderToken) return;
      const pre = block.parentElement;
      if (!pre) continue;
      try {
        const source = await resolveLocalMermaidImageReferences(
          block.textContent ?? "",
          isDesktop()
            ? (resource) => api.loadResource(id, resource)
            : undefined,
        );
        if (token !== renderToken) return;
        const result = await renderMermaid(
          source,
          {
            startOnLoad: false,
            securityLevel: "strict",
            theme: currentTheme === "dark" ? "dark" : "neutral",
            fontFamily: currentFont,
          },
          "inkflow-mermaid",
          () => token === renderToken,
        );
        if (token !== renderToken) return;
        const figure = document.createElement("figure");
        figure.className = "mermaid-diagram";
        figure.innerHTML = remote ? result.svg : blockRemoteImageRequests(result.svg);
        pre.replaceWith(figure);
      } catch (cause) {
        if (token !== renderToken) return;
        pre.classList.add("render-error");
        pre.setAttribute("data-error", cause instanceof Error ? cause.message : "Mermaid render failed");
      }
    }
  }

  function handleClick(event: MouseEvent): void {
    const anchor = (event.target as Element).closest<HTMLAnchorElement>("a[href]");
    if (!anchor) return;
    const href = anchor.getAttribute("href") ?? "";
    if (/^https?:\/\//i.test(href)) {
      event.preventDefault();
      if (isDesktop()) void openUrl(href);
      else window.open(href, "_blank", "noopener,noreferrer");
    }
  }

  export function getHtml(): string {
    return container?.innerHTML ?? html;
  }
</script>

<div
  class="preview-scroller"
  style={`--preview-width:${pageWidth}px;--preview-size:${fontSize}px;--preview-line:${lineHeight};--preview-font:${editorFont}`}
>
  {#if error}<div class="render-warning">{error}</div>{/if}
  <article class="markdown-preview" bind:this={container}>{@html html}</article>
</div>

<style>
  .preview-scroller{height:100%;overflow:auto}
  .markdown-preview{width:100%;max-width:var(--preview-width);min-height:calc(100vh - 126px);margin:0 auto;padding:56px 48px 35vh;color:var(--ink);font:var(--preview-size)/var(--preview-line) var(--preview-font)}
  .render-warning{position:sticky;top:12px;z-index:2;max-width:680px;margin:12px auto;padding:9px 12px;border:1px solid var(--danger);border-radius:8px;background:var(--panel);color:var(--danger);font-size:13px}
  :global(.markdown-preview h1),:global(.markdown-preview h2),:global(.markdown-preview h3),:global(.markdown-preview h4){line-height:1.3;margin:1.65em 0 .65em;font-weight:650}
  :global(.markdown-preview h1){font-size:2em}:global(.markdown-preview h2){font-size:1.55em;padding-bottom:.24em;border-bottom:1px solid var(--line)}:global(.markdown-preview h3){font-size:1.3em}
  :global(.markdown-preview p),:global(.markdown-preview ul),:global(.markdown-preview ol),:global(.markdown-preview pre),:global(.markdown-preview table){margin:1em 0}
  :global(.markdown-preview blockquote){margin:1em 0;padding:.15em 1em;border-left:3px solid var(--accent-soft);color:var(--muted)}
  :global(.markdown-preview code){padding:.12em .35em;border-radius:4px;background:var(--code-bg);font-family:var(--code-font);font-size:.9em}
  :global(.markdown-preview pre){overflow:auto;padding:1em;border:1px solid var(--line);border-radius:8px;background:var(--code-block)}
  :global(.markdown-preview pre code){padding:0;background:none}
  :global(.markdown-preview table){width:100%;border-collapse:collapse}:global(.markdown-preview th),:global(.markdown-preview td){padding:.45em .7em;border:1px solid var(--line);text-align:left}
  :global(.markdown-preview img),:global(.markdown-preview svg){max-width:100%;height:auto}:global(.markdown-preview img){display:block;margin:1.3em auto;border-radius:8px}
  :global(.markdown-preview img.remote-blocked),:global(.markdown-preview img.resource-missing){min-height:54px;padding:14px;border:1px dashed var(--line);color:var(--muted)}
  :global(.markdown-preview a){color:var(--link);text-decoration:none}:global(.markdown-preview a:hover){text-decoration:underline}
  :global(.markdown-preview .mermaid-diagram){margin:1.4em 0;text-align:center}:global(.markdown-preview pre.render-error::before){display:block;margin-bottom:.7em;color:var(--danger);content:attr(data-error)}
  :global(.markdown-preview hr){margin:2em 0;border:0;border-top:1px solid var(--line)}
  @media print{.preview-scroller{overflow:visible}.markdown-preview{max-width:none;min-height:0;padding:0}.render-warning{display:none}}
</style>
