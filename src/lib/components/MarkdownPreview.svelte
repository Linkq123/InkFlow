<script lang="ts">
  import { onMount, tick } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api, isDesktop } from "../api/client";
  import { renderInWorker } from "../markdown/render-service";

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

  onMount(() => {
    container.addEventListener("click", handleClick);
    return () => container.removeEventListener("click", handleClick);
  });

  $: void refresh(value, documentId, allowRemoteImages, theme);

  async function refresh(markdown: string, id: string, remote: boolean, currentTheme: string): Promise<void> {
    const token = ++renderToken;
    try {
      const rendered = await renderInWorker(markdown);
      if (token !== renderToken) return;
      html = rendered;
      error = "";
      await tick();
      await hydrateImages(id, remote);
      await hydrateMermaid(currentTheme);
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === "AbortError") return;
      error = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function hydrateImages(id: string, remote: boolean): Promise<void> {
    if (!container) return;
    const images = Array.from(container.querySelectorAll<HTMLImageElement>("img"));
    await Promise.all(images.map(async (image) => {
      const source = image.getAttribute("src") ?? "";
      if (/^https?:\/\//i.test(source)) {
        if (!remote) {
          image.removeAttribute("src");
          image.classList.add("remote-blocked");
          image.alt = `Remote image blocked: ${image.alt || source}`;
        }
        return;
      }
      if (!isDesktop() || source.startsWith("data:") || !source) return;
      try {
        image.src = await api.loadResource(id, source);
      } catch {
        image.classList.add("resource-missing");
        image.alt = `Missing image: ${image.alt || source}`;
      }
    }));
  }

  async function hydrateMermaid(currentTheme: string): Promise<void> {
    if (!container) return;
    const blocks = Array.from(container.querySelectorAll<HTMLElement>("pre > code.language-mermaid"));
    if (blocks.length === 0) return;
    const mermaid = (await import("mermaid")).default;
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: currentTheme === "dark" ? "dark" : "neutral",
      fontFamily: editorFont,
    });
    for (const block of blocks) {
      const pre = block.parentElement;
      if (!pre) continue;
      try {
        const id = `inkflow-mermaid-${crypto.randomUUID()}`;
        const result = await mermaid.render(id, block.textContent ?? "");
        const figure = document.createElement("figure");
        figure.className = "mermaid-diagram";
        figure.innerHTML = result.svg;
        pre.replaceWith(figure);
      } catch (cause) {
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
