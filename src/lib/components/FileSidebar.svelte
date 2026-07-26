<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    ChevronDown,
    ChevronRight,
    FilePlus2,
    FileText,
    Folder,
    FolderOpen,
    FolderPlus,
    MoreHorizontal,
    RefreshCw,
  } from "@lucide/svelte";
  import type { WorkspaceEntry, WorkspaceSnapshot } from "../api/types";
  import { translate, type Locale } from "../i18n";

  export let locale: Locale = "zh-CN";
  export let workspace: WorkspaceSnapshot | null;
  export let activePath: string | null = null;
  export let onOpen: (entry: WorkspaceEntry) => void;
  export let onCreate: (isDir: boolean) => void;
  export let onRefresh: () => void;
  export let onRename: (entry: WorkspaceEntry) => void;
  export let onDelete: (entry: WorkspaceEntry) => void;

  let collapsed = new Set<string>();
  let menuPath: string | null = null;
  let menuReturnFocus: HTMLButtonElement | null = null;
  let sidebar: HTMLElement;
  $: visibleEntries = workspace?.entries.filter((entry) => !hasCollapsedAncestor(entry)) ?? [];

  function hasCollapsedAncestor(entry: WorkspaceEntry): boolean {
    for (const directory of collapsed) {
      if (entry.path !== directory && isInside(entry.path, directory)) return true;
    }
    return false;
  }

  function isInside(path: string, directory: string): boolean {
    const separator = path.includes("\\") ? "\\" : "/";
    const prefix = directory.endsWith(separator) ? directory : `${directory}${separator}`;
    return path.startsWith(prefix);
  }

  function activate(entry: WorkspaceEntry): void {
    menuPath = null;
    if (entry.isDir) {
      if (collapsed.has(entry.path)) collapsed.delete(entry.path);
      else collapsed.add(entry.path);
      collapsed = new Set(collapsed);
    } else {
      onOpen(entry);
    }
  }

  function entryKeydown(event: KeyboardEvent, entry: WorkspaceEntry): void {
    if (event.key === "F2") {
      event.preventDefault();
      onRename(entry);
    } else if (entry.isDir && event.key === "ArrowRight" && collapsed.has(entry.path)) {
      event.preventDefault();
      collapsed.delete(entry.path);
      collapsed = new Set(collapsed);
    } else if (entry.isDir && event.key === "ArrowLeft" && !collapsed.has(entry.path)) {
      event.preventDefault();
      collapsed.add(entry.path);
      collapsed = new Set(collapsed);
    }
  }

  function toggleEntryMenu(path: string, trigger: HTMLButtonElement): void {
    if (menuPath === path) {
      closeEntryMenu(false);
      return;
    }
    menuPath = path;
    menuReturnFocus = trigger;
    void tick().then(() => focusMenuItem(0));
  }

  function closeEntryMenu(restoreFocus: boolean): void {
    const trigger = menuReturnFocus;
    menuPath = null;
    menuReturnFocus = null;
    if (restoreFocus) void tick().then(() => {
      if (trigger?.isConnected) trigger.focus();
    });
  }

  function runMenuAction(action: () => void): void {
    const trigger = menuReturnFocus;
    menuPath = null;
    menuReturnFocus = null;
    action();
    void tick().then(() => {
      if (trigger?.isConnected) trigger.focus();
    });
  }

  function menuKeydown(event: KeyboardEvent): void {
    if (event.isComposing) return;
    const menu = event.currentTarget as HTMLElement;
    const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    let next = current;
    if (event.key === "ArrowDown") next = Math.min(current + 1, items.length - 1);
    else if (event.key === "ArrowUp") next = Math.max(current - 1, 0);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = items.length - 1;
    else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeEntryMenu(true);
      return;
    } else return;
    event.preventDefault();
    focusMenuItem(Math.max(0, next), items);
  }

  function focusMenuItem(index: number, items?: HTMLButtonElement[]): void {
    const available = items ?? Array.from(
      sidebar?.querySelectorAll<HTMLButtonElement>('.entry-menu [role="menuitem"]') ?? [],
    );
    const item = available[index];
    item?.focus();
    item?.scrollIntoView?.({ block: "nearest" });
  }

  onMount(() => {
    const pointer = (event: MouseEvent) => {
      const target = event.target instanceof Element ? event.target : null;
      if (menuPath && !target?.closest(".entry-menu,.row-menu")) closeEntryMenu(false);
    };
    const keydown = (event: KeyboardEvent) => {
      if (menuPath && !event.isComposing && event.key === "Escape") {
        event.preventDefault();
        closeEntryMenu(true);
      }
    };
    window.addEventListener("mousedown", pointer);
    window.addEventListener("keydown", keydown);
    return () => {
      window.removeEventListener("mousedown", pointer);
      window.removeEventListener("keydown", keydown);
    };
  });
</script>

<aside class="file-sidebar" aria-label="Workspace files" bind:this={sidebar}>
  <header>
    <div class="workspace-name" title={workspace?.root ?? ""}>
      <FolderOpen size={15}/><span>{workspace?.name ?? translate(locale, "files")}</span>
    </div>
    <div class="sidebar-actions">
      <button title="New document" on:click={() => onCreate(false)} disabled={!workspace}><FilePlus2 size={15}/></button>
      <button title="New folder" on:click={() => onCreate(true)} disabled={!workspace}><FolderPlus size={15}/></button>
      <button title="Refresh" on:click={onRefresh} disabled={!workspace}><RefreshCw size={14}/></button>
    </div>
  </header>

  {#if !workspace}
    <div class="empty-sidebar"><Folder size={28}/><p>{translate(locale, "browseFolderHint")}</p></div>
  {:else}
    <div class="file-list">
      {#each visibleEntries as entry (entry.path)}
        <div
          class:active={!entry.isDir && entry.path === activePath}
          class="file-row"
          style={`--depth:${entry.depth}`}
          title={entry.path}
        >
          <button class="file-main" aria-expanded={entry.isDir ? !collapsed.has(entry.path) : undefined} on:click={() => activate(entry)} on:keydown={(event) => entryKeydown(event, entry)}>
            {#if entry.isDir}
              {#if collapsed.has(entry.path)}<ChevronRight size={13}/>{:else}<ChevronDown size={13}/>{/if}
              <Folder size={15}/>
            {:else}
              <span class="file-spacer"></span><FileText size={15}/>
            {/if}
            <span>{entry.name}</span>
          </button>
          <button class="row-menu" title="More" aria-haspopup="menu" aria-expanded={menuPath === entry.path} on:click={(event) => toggleEntryMenu(entry.path, event.currentTarget)}><MoreHorizontal size={14}/></button>
          {#if menuPath === entry.path}
            <div class="entry-menu" role="menu" tabindex="-1" on:keydown={menuKeydown}>
              <button role="menuitem" on:click={() => runMenuAction(() => onRename(entry))}>{translate(locale, "rename")}</button>
              <button role="menuitem" class="danger" on:click={() => runMenuAction(() => onDelete(entry))}>{translate(locale, "moveTrash")}</button>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</aside>

<style>
  .file-sidebar{width:100%;height:100%;overflow:hidden;border-right:1px solid var(--line);background:var(--sidebar);font-size:13px}
  header{display:flex;align-items:center;justify-content:space-between;height:42px;padding:0 8px 0 12px;border-bottom:1px solid var(--line)}
  .workspace-name{display:flex;min-width:0;align-items:center;gap:7px;font-weight:650}.workspace-name span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .sidebar-actions{display:flex}.sidebar-actions button,.row-menu{display:grid;width:28px;height:28px;place-items:center;border:0;border-radius:6px;background:transparent;color:var(--muted);cursor:pointer}.sidebar-actions button:hover,.row-menu:hover{background:var(--hover);color:var(--ink)}
  button:disabled{opacity:.35;cursor:default}
  .file-list{height:calc(100% - 42px);overflow:auto;padding:5px}
  .file-row{position:relative;display:flex;align-items:center;height:29px;border-radius:6px;padding-left:calc(var(--depth) * 14px)}.file-row:hover,.file-row.active{background:var(--hover)}.file-row.active{color:var(--accent-strong)}
  .file-main{display:flex;min-width:0;flex:1;align-items:center;gap:6px;height:100%;padding:0 4px;border:0;background:transparent;color:inherit;text-align:left;cursor:pointer}.file-main span:last-child{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.file-spacer{width:13px}
  .row-menu{visibility:hidden;flex:none}.file-row:hover .row-menu,.file-row:focus-within .row-menu{visibility:visible}
  .entry-menu{position:absolute;z-index:50;top:26px;right:2px;width:128px;padding:5px;border:1px solid var(--line);border-radius:8px;background:var(--panel);box-shadow:var(--shadow-lg)}.entry-menu button{width:100%;padding:7px 8px;border:0;border-radius:5px;background:transparent;color:var(--ink);text-align:left;cursor:pointer}.entry-menu button:hover{background:var(--hover)}.entry-menu .danger{color:var(--danger)}
  .empty-sidebar{display:grid;height:calc(100% - 42px);place-content:center;justify-items:center;color:var(--muted)}.empty-sidebar p{margin:9px 0;font-size:12px}
</style>
