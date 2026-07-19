<script lang="ts">
  import { Search, X } from "@lucide/svelte";
  import type { SearchHit } from "../api/types";
  import { translate, type Locale } from "../i18n";

  export let locale: Locale = "zh-CN";
  export let open = false;
  export let workspaceName = "";
  export let searching = false;
  export let results: SearchHit[] = [];
  export let onSearch: (query: string) => void;
  export let onOpen: (hit: SearchHit) => void;
  export let onClose: () => void;
  let query = "";
  let timer: ReturnType<typeof setTimeout> | null = null;

  $: if (open) setTimeout(() => document.querySelector<HTMLInputElement>("#workspace-search-input")?.focus());

  function update(): void {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => onSearch(query), 180);
  }
</script>

{#if open}
  <section class="search-panel" aria-label="Workspace search">
    <header><strong>{translate(locale, "search")}</strong><span>{workspaceName}</span><button title={translate(locale, "close")} on:click={onClose}><X size={15}/></button></header>
    <div class="search-input"><Search size={15}/><input id="workspace-search-input" bind:value={query} on:input={update} placeholder={translate(locale, "searchPlaceholder")}/></div>
    <div class="results">
      {#if searching}<div class="state">{translate(locale, "searching")}</div>
      {:else if query && results.length === 0}<div class="state">{translate(locale, "noResults")}</div>{/if}
      {#each results as result}
        <button on:click={() => onOpen(result)}>
          <div><strong>{result.relativePath}</strong><span>{result.line}:{result.column}</span></div>
          <p>{result.preview}</p>
        </button>
      {/each}
    </div>
  </section>
{/if}

<style>
  .search-panel{position:absolute;z-index:45;top:50px;right:12px;width:min(430px,calc(100vw - 24px));max-height:calc(100vh - 74px);overflow:hidden;border:1px solid var(--line);border-radius:11px;background:var(--panel);box-shadow:var(--shadow-xl)}
  header{display:grid;grid-template-columns:auto 1fr auto;align-items:center;gap:8px;height:42px;padding:0 10px 0 13px;border-bottom:1px solid var(--line)}header span{overflow:hidden;color:var(--muted);font-size:12px;text-overflow:ellipsis;white-space:nowrap}header button{display:grid;width:27px;height:27px;place-items:center;border:0;border-radius:6px;background:transparent;color:var(--muted);cursor:pointer}header button:hover{background:var(--hover)}
  .search-input{display:flex;align-items:center;gap:8px;margin:9px;padding:8px 10px;border:1px solid var(--line);border-radius:7px;color:var(--muted)}.search-input:focus-within{border-color:var(--accent)}.search-input input{min-width:0;flex:1;border:0;outline:0;background:transparent;color:var(--ink)}
  .results{max-height:calc(100vh - 190px);overflow:auto;padding:0 7px 8px}.results button{display:block;width:100%;padding:9px;border:0;border-radius:7px;background:transparent;color:var(--ink);text-align:left;cursor:pointer}.results button:hover{background:var(--hover)}.results button div{display:flex;justify-content:space-between;gap:8px}.results button div span{color:var(--muted);font-size:11px}.results p{overflow:hidden;margin:4px 0 0;color:var(--muted);font-size:12px;text-overflow:ellipsis;white-space:nowrap}.state{padding:24px;text-align:center;color:var(--muted);font-size:12px}
</style>
