<script lang="ts">
  import { onMount } from "svelte";
  import { Command, Search } from "@lucide/svelte";
  import type { PaletteCommand } from "./palette";

  export let open = false;
  export let commands: PaletteCommand[] = [];
  export let onClose: () => void;
  export let placeholder = "输入命令…";
  export let emptyText = "没有匹配的命令";
  export let ariaLabel = "Command palette";
  let query = "";
  let input: HTMLInputElement;
  let active = 0;

  $: filtered = commands.filter((command) => `${command.label} ${command.section ?? ""}`.toLowerCase().includes(query.toLowerCase()));
  $: if (open) { query = ""; active = 0; setTimeout(() => input?.focus()); }

  onMount(() => {
    const listener = (event: KeyboardEvent) => {
      if (!open) return;
      if (event.key === "Escape") onClose();
      if (event.key === "ArrowDown") { event.preventDefault(); active = Math.min(active + 1, filtered.length - 1); }
      if (event.key === "ArrowUp") { event.preventDefault(); active = Math.max(active - 1, 0); }
      if (event.key === "Enter" && filtered[active]) { event.preventDefault(); void execute(filtered[active]); }
    };
    window.addEventListener("keydown", listener);
    return () => window.removeEventListener("keydown", listener);
  });

  async function execute(command: PaletteCommand): Promise<void> {
    onClose();
    await command.run();
  }
</script>

{#if open}
  <div class="overlay" role="presentation" on:mousedown={(event) => event.target === event.currentTarget && onClose()}>
    <div class="palette" role="dialog" aria-modal="true" aria-label={ariaLabel} tabindex="-1">
      <div class="search-box"><Search size={17}/><input bind:this={input} bind:value={query} {placeholder} aria-label={ariaLabel}/></div>
      <div class="command-list">
        {#if filtered.length === 0}<div class="empty">{emptyText}</div>{/if}
        {#each filtered as command, index (command.id)}
          <button class:active={index === active} on:mouseenter={() => active = index} on:click={() => execute(command)}>
            <Command size={15}/><span>{command.label}</span>{#if command.shortcut}<kbd>{command.shortcut}</kbd>{/if}
          </button>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay{position:fixed;z-index:100;inset:0;background:rgba(20,25,30,.18);backdrop-filter:blur(2px)}
  .palette{width:min(590px,calc(100vw - 32px));margin:12vh auto 0;overflow:hidden;border:1px solid var(--line);border-radius:12px;background:var(--panel);box-shadow:var(--shadow-xl)}
  .search-box{display:flex;align-items:center;gap:10px;height:48px;padding:0 15px;border-bottom:1px solid var(--line);color:var(--muted)}.search-box input{min-width:0;flex:1;border:0;outline:0;background:transparent;color:var(--ink);font:15px var(--ui-font)}
  .command-list{max-height:360px;overflow:auto;padding:6px}.command-list button{display:grid;grid-template-columns:22px 1fr auto;align-items:center;width:100%;padding:9px 10px;border:0;border-radius:7px;background:transparent;color:var(--ink);text-align:left;cursor:pointer}.command-list button.active{background:var(--hover)}
  kbd{padding:2px 6px;border:1px solid var(--line);border-radius:5px;background:var(--subtle);color:var(--muted);font:11px var(--ui-font)}.empty{padding:24px;text-align:center;color:var(--muted)}
</style>
