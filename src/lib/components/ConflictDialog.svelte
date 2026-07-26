<script lang="ts">
  import { translate, type Locale } from "../i18n";
  import { focusTrap } from "./focus-trap";

  export let locale: Locale = "zh-CN";
  export let open = false;
  export let title = "";
  export let localContent = "";
  export let diskContent = "";
  export let onReload: () => void;
  export let onSaveAs: () => void;
  export let onClose: () => void;
</script>

{#if open}
  <div class="overlay" role="presentation" on:mousedown={(event) => event.target === event.currentTarget && onClose()}>
    <div class="conflict-dialog" role="dialog" aria-modal="true" aria-label="Compare external changes" tabindex="-1" use:focusTrap={{ onClose }}>
      <header>
        <div><strong>{translate(locale, "compareChanges")}</strong><span>{title}</span></div>
        <button class="quiet" on:click={onClose}>{translate(locale, "close")}</button>
      </header>
      <div class="comparison">
        <label><span>{translate(locale, "localVersion")}</span><textarea readonly value={localContent}></textarea></label>
        <label><span>{translate(locale, "diskVersion")}</span><textarea readonly value={diskContent}></textarea></label>
      </div>
      <footer>
        <p>{translate(locale, "conflictHelp")}</p>
        <button on:click={onSaveAs}>{translate(locale, "saveLocalAs")}</button>
        <button class="primary" on:click={onReload}>{translate(locale, "reloadDisk")}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay{position:fixed;z-index:120;inset:0;display:grid;place-items:center;padding:24px;background:rgba(15,20,18,.4)}
  .conflict-dialog{display:grid;width:min(1040px,100%);height:min(720px,calc(100vh - 48px));grid-template-rows:auto 1fr auto;overflow:hidden;border:1px solid var(--line);border-radius:13px;background:var(--panel);box-shadow:var(--shadow-xl)}
  header,footer{display:flex;align-items:center;gap:10px;padding:12px 14px;border-bottom:1px solid var(--line)}header{justify-content:space-between}header div{display:flex;min-width:0;align-items:baseline;gap:9px}header span{overflow:hidden;color:var(--muted);font-size:12px;text-overflow:ellipsis;white-space:nowrap}
  .comparison{display:grid;min-height:0;grid-template-columns:1fr 1fr;gap:1px;background:var(--line)}label{display:grid;min-width:0;min-height:0;grid-template-rows:34px 1fr;background:var(--panel)}label>span{padding:8px 12px;color:var(--muted);font-size:12px}textarea{width:100%;height:100%;resize:none;border:0;border-top:1px solid var(--line-soft);outline:0;padding:14px;background:var(--code-block);color:var(--ink);font:12px/1.65 var(--code-font);tab-size:2;white-space:pre}
  footer{justify-content:flex-end;border-top:1px solid var(--line);border-bottom:0}footer p{margin:0 auto 0 0;color:var(--muted);font-size:12px}button{padding:7px 11px;border:1px solid var(--line);border-radius:7px;background:var(--subtle);color:var(--ink);cursor:pointer}button:hover{background:var(--hover)}button.primary{border-color:var(--accent);background:var(--accent);color:#fff}.quiet{border:0;background:transparent;color:var(--muted)}
  @media(max-width:760px){.comparison{grid-template-columns:1fr;grid-template-rows:1fr 1fr}footer p{display:none}}
</style>
