<script lang="ts">
  import { ListTree } from "@lucide/svelte";
  import type { OutlineItem } from "../stats";
  import { translate, type Locale } from "../i18n";

  export let locale: Locale = "zh-CN";
  export let items: OutlineItem[] = [];
  export let onSelect: (item: OutlineItem) => void;
</script>

<aside class="outline-sidebar" aria-label="Document outline">
  <header><ListTree size={15}/><span>{translate(locale, "outline")}</span></header>
  {#if items.length === 0}
    <div class="empty">{translate(locale, "outlineEmpty")}</div>
  {:else}
    <nav>
      {#each items as item}
        <button style={`--level:${item.level}`} title={item.text} on:click={() => onSelect(item)}>{item.text}</button>
      {/each}
    </nav>
  {/if}
</aside>

<style>
  .outline-sidebar{width:100%;height:100%;overflow:hidden;border-left:1px solid var(--line);background:var(--sidebar);font-size:13px}
  header{display:flex;align-items:center;gap:7px;height:42px;padding:0 12px;border-bottom:1px solid var(--line);font-weight:650}
  nav{height:calc(100% - 42px);overflow:auto;padding:7px}
  nav button{display:block;width:100%;overflow:hidden;padding:6px 7px 6px calc(7px + (var(--level) - 1) * 11px);border:0;border-radius:6px;background:transparent;color:var(--muted);text-align:left;text-overflow:ellipsis;white-space:nowrap;cursor:pointer}nav button:hover{background:var(--hover);color:var(--ink)}
  .empty{padding:18px 14px;color:var(--muted);font-size:12px;line-height:1.6}
</style>
