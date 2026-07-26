<script lang="ts">
  import { History, RotateCcw, Trash2, X } from "@lucide/svelte";
  import type { RecoveryEntry } from "../api/types";
  import { translate, type Locale } from "../i18n";
  import { focusTrap } from "./focus-trap";

  export let locale: Locale = "zh-CN";
  export let open = false;
  export let entries: RecoveryEntry[] = [];
  export let loading = false;
  export let onRestore: (entry: RecoveryEntry) => void;
  export let onDelete: (entry: RecoveryEntry) => void;
  export let onClose: () => void;

  $: formatter = new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "short" });

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  }
</script>

{#if open}
  <div class="overlay" role="presentation" on:mousedown={(event) => event.target === event.currentTarget && onClose()}>
    <div class="dialog" role="dialog" aria-modal="true" aria-label="Recovery history" tabindex="-1" use:focusTrap={{ onClose }}>
      <header><div><History size={18}/><h2>{translate(locale, "recovery")}</h2></div><button title={translate(locale, "close")} on:click={onClose}><X size={17}/></button></header>
      <div class="entries">
        {#if loading}<div class="empty">{translate(locale, "recoveryLoading")}</div>
        {:else if entries.length === 0}<div class="empty"><History size={30}/><p>{translate(locale, "recoveryEmpty")}</p></div>{/if}
        {#each entries as entry (entry.id)}
          <article>
            <div class="entry-info"><strong>{entry.title}</strong><span>{formatter.format(new Date(entry.createdAt))} · {entry.kind === "history" ? translate(locale, "history") : translate(locale, "draft")} · {formatSize(entry.size)}</span>{#if entry.path}<small>{entry.path}</small>{/if}</div>
            <button title={translate(locale, "restore")} on:click={() => onRestore(entry)}><RotateCcw size={15}/><span>{translate(locale, "restore")}</span></button>
            <button class="delete" title="Delete" on:click={() => onDelete(entry)}><Trash2 size={15}/></button>
          </article>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay{position:fixed;z-index:110;inset:0;background:rgba(20,25,30,.3)}
  .dialog{width:min(680px,calc(100vw - 32px));max-height:78vh;margin:9vh auto 0;overflow:hidden;border:1px solid var(--line);border-radius:13px;background:var(--panel);box-shadow:var(--shadow-xl)}
  header{display:flex;align-items:center;justify-content:space-between;height:52px;padding:0 14px 0 17px;border-bottom:1px solid var(--line)}header div{display:flex;align-items:center;gap:9px}h2{margin:0;font-size:16px}header button{display:grid;width:30px;height:30px;place-items:center;border:0;border-radius:7px;background:transparent;color:var(--muted);cursor:pointer}header button:hover{background:var(--hover)}
  .entries{max-height:calc(78vh - 52px);overflow:auto;padding:8px}.entries article{display:grid;grid-template-columns:1fr auto auto;align-items:center;gap:8px;padding:10px;border-radius:8px}.entries article:hover{background:var(--hover)}.entry-info{display:flex;min-width:0;flex-direction:column;gap:3px}.entry-info span,.entry-info small{overflow:hidden;color:var(--muted);font-size:11px;text-overflow:ellipsis;white-space:nowrap}.entries article>button{display:flex;align-items:center;gap:5px;padding:7px 9px;border:1px solid var(--line);border-radius:7px;background:var(--panel);color:var(--ink);cursor:pointer}.entries article>button:hover{background:var(--subtle)}.entries article>.delete{border-color:transparent;color:var(--danger)}
  .empty{display:grid;min-height:190px;place-content:center;justify-items:center;color:var(--muted)}.empty p{margin:8px}
</style>
