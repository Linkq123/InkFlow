<script lang="ts">
  import { onMount } from "svelte";
  import { Search, X } from "@lucide/svelte";
  import type { SearchHit } from "../api/types";
  import { translate, type Locale } from "../i18n";
  import { focusTrap } from "./focus-trap";

  export let locale: Locale = "zh-CN";
  export let open = false;
  export let workspaceName = "";
  export let searching = false;
  export let results: SearchHit[] = [];
  export let resultQuery = "";
  export let onSearch: (query: string) => void | Promise<void>;
  export let onOpen: (hit: SearchHit) => void;
  export let onClose: () => void;
  let query = "";
  let inputValue = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let input: HTMLInputElement;
  let panel: HTMLElement | null = null;
  let active = 0;
  let composing = false;
  let waiting = false;
  let resultElements: HTMLButtonElement[] = [];

  $: visibleResults = resultQuery === query ? results : [];
  $: if (active >= visibleResults.length) active = Math.max(0, visibleResults.length - 1);
  $: if (open && resultElements[active]) resultElements[active].scrollIntoView?.({ block: "nearest" });

  function update(): void {
    if (composing) return;
    if (timer) clearTimeout(timer);
    timer = null;
    active = 0;
    if (!query.trim()) {
      waiting = false;
      void onSearch(query);
      return;
    }
    waiting = true;
    timer = setTimeout(() => {
      timer = null;
      waiting = false;
      void onSearch(query);
    }, 180);
  }

  function closePanel(): void {
    if (timer) clearTimeout(timer);
    timer = null;
    waiting = false;
    onClose();
  }

  function keydown(event: KeyboardEvent): void {
    if (event.isComposing || composing) return;
    if (!visibleResults.length && ["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) {
      event.preventDefault();
      active = 0;
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      active = Math.min(active + 1, visibleResults.length - 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      active = Math.max(active - 1, 0);
    } else if (event.key === "Enter" && !waiting && !searching && visibleResults[active]) {
      event.preventDefault();
      onOpen(visibleResults[active]);
    }
  }

  onMount(() => {
    const pointer = (event: MouseEvent) => {
      if (!open || !panel || !(event.target instanceof Node)) return;
      const target = event.target instanceof Element ? event.target : null;
      if (panel.contains(event.target) || target?.closest("[data-workspace-search-trigger]")) return;
      closePanel();
    };
    window.addEventListener("mousedown", pointer);
    return () => {
      window.removeEventListener("mousedown", pointer);
      if (timer) clearTimeout(timer);
    };
  });
</script>

{#if open}
  <div bind:this={panel} class="search-panel" role="dialog" tabindex="-1" aria-label="Workspace search" on:keydown={keydown} use:focusTrap={{ onClose: closePanel, initialFocus: "input" }}>
    <header><strong>{translate(locale, "search")}</strong><span>{workspaceName}</span><button title={translate(locale, "close")} on:click={closePanel}><X size={15}/></button></header>
    <div class="search-input"><Search size={15}/><input bind:this={input} id="workspace-search-input" bind:value={inputValue} on:input={() => { if (!composing) { query = inputValue; update(); } }} on:compositionstart={() => composing = true} on:compositionend={() => { composing = false; query = inputValue; update(); }} placeholder={translate(locale, "searchPlaceholder")}/></div>
    <div class="results">
      {#if searching || waiting}<div class="state">{translate(locale, "searching")}</div>
      {:else if query && visibleResults.length === 0}<div class="state">{translate(locale, "noResults")}</div>{/if}
      {#each visibleResults as result, index}
        <button bind:this={resultElements[index]} class:active={index === active} aria-current={index === active ? "true" : undefined} on:focus={() => active = index} on:mouseenter={() => active = index} on:click={() => onOpen(result)}>
          <div><strong>{result.relativePath}</strong><span>{result.line}:{result.column}</span></div>
          <p>{result.preview}</p>
        </button>
      {/each}
    </div>
  </div>
{/if}

<style>
  .search-panel{position:absolute;z-index:45;top:50px;right:12px;width:min(430px,calc(100vw - 24px));max-height:calc(100vh - 74px);overflow:hidden;border:1px solid var(--line);border-radius:11px;background:var(--panel);box-shadow:var(--shadow-xl)}
  header{display:grid;grid-template-columns:auto 1fr auto;align-items:center;gap:8px;height:42px;padding:0 10px 0 13px;border-bottom:1px solid var(--line)}header span{overflow:hidden;color:var(--muted);font-size:12px;text-overflow:ellipsis;white-space:nowrap}header button{display:grid;width:27px;height:27px;place-items:center;border:0;border-radius:6px;background:transparent;color:var(--muted);cursor:pointer}header button:hover{background:var(--hover)}
  .search-input{display:flex;align-items:center;gap:8px;margin:9px;padding:8px 10px;border:1px solid var(--line);border-radius:7px;color:var(--muted)}.search-input:focus-within{border-color:var(--accent)}.search-input input{min-width:0;flex:1;border:0;outline:0;background:transparent;color:var(--ink)}
  .results{max-height:calc(100vh - 190px);overflow:auto;padding:0 7px 8px}.results button{display:block;width:100%;padding:9px;border:0;border-radius:7px;background:transparent;color:var(--ink);text-align:left;cursor:pointer}.results button:hover,.results button.active{background:var(--hover)}.results button div{display:flex;justify-content:space-between;gap:8px}.results button div span{color:var(--muted);font-size:11px}.results p{overflow:hidden;margin:4px 0 0;color:var(--muted);font-size:12px;text-overflow:ellipsis;white-space:nowrap}.state{padding:24px;text-align:center;color:var(--muted);font-size:12px}
</style>
