<script lang="ts">
  import { X } from "@lucide/svelte";
  import type { SettingsV1 } from "../api/types";
  import { translate, type Locale } from "../i18n";

  export let locale: Locale = "zh-CN";
  export let open = false;
  export let settings: SettingsV1;
  export let onSave: (settings: SettingsV1) => void;
  export let onClose: () => void;
  let draft: SettingsV1;

  $: if (open) draft = structuredClone(settings);
</script>

{#if open && draft}
  <div class="overlay" role="presentation" on:mousedown={(event) => event.target === event.currentTarget && onClose()}>
    <div class="dialog" role="dialog" aria-modal="true" aria-label="Settings" tabindex="-1">
      <header><h2>{translate(locale, "settings")}</h2><button title={translate(locale, "close")} on:click={onClose}><X size={17}/></button></header>
      <div class="settings-body">
        <label><span>{translate(locale, "settingsTheme")}</span><select bind:value={draft.theme}><option value="system">{translate(locale, "followSystem")}</option><option value="light">{translate(locale, "light")}</option><option value="dark">{translate(locale, "dark")}</option></select></label>
        <label><span>{translate(locale, "settingsLanguage")}</span><select bind:value={draft.locale}><option value="system">{translate(locale, "followSystem")}</option><option value="zh-CN">{translate(locale, "simplifiedChinese")}</option><option value="en-US">English</option></select></label>
        <label><span>{translate(locale, "bodySize")}</span><div class="range"><input type="range" min="12" max="28" step="1" bind:value={draft.fontSize}/><output>{draft.fontSize}px</output></div></label>
        <label><span>{translate(locale, "pageWidth")}</span><div class="range"><input type="range" min="560" max="1200" step="20" bind:value={draft.pageWidth}/><output>{draft.pageWidth}px</output></div></label>
        <label><span>{translate(locale, "lineHeight")}</span><div class="range"><input type="range" min="1.2" max="2.2" step="0.05" bind:value={draft.lineHeight}/><output>{Number(draft.lineHeight).toFixed(2)}</output></div></label>
        <label><span>{translate(locale, "bodyFont")}</span><input type="text" bind:value={draft.editorFont}/></label>
        <label><span>{translate(locale, "codeFont")}</span><input type="text" bind:value={draft.codeFont}/></label>
        <label><span>{translate(locale, "autosaveDelay")}</span><div class="range"><input type="range" min="250" max="3000" step="250" bind:value={draft.autosaveDelayMs}/><output>{draft.autosaveDelayMs}ms</output></div></label>
      </div>
      <footer><button class="secondary" on:click={onClose}>{translate(locale, "cancel")}</button><button class="primary" on:click={() => onSave(draft)}>{translate(locale, "saveSettings")}</button></footer>
    </div>
  </div>
{/if}

<style>
  .overlay{position:fixed;z-index:110;inset:0;background:rgba(20,25,30,.25);backdrop-filter:blur(3px)}
  .dialog{display:flex;width:min(620px,calc(100vw - 32px));max-height:82vh;flex-direction:column;margin:8vh auto 0;overflow:hidden;border:1px solid var(--line);border-radius:13px;background:var(--panel);box-shadow:var(--shadow-xl)}
  header{display:flex;align-items:center;justify-content:space-between;height:52px;padding:0 16px;border-bottom:1px solid var(--line)}h2{margin:0;font-size:16px}header button{display:grid;width:30px;height:30px;place-items:center;border:0;border-radius:7px;background:transparent;color:var(--muted);cursor:pointer}header button:hover{background:var(--hover)}
  .settings-body{overflow:auto;padding:10px 18px}.settings-body>label{display:grid;grid-template-columns:145px 1fr;align-items:center;min-height:49px;border-bottom:1px solid var(--line-soft);gap:16px}.settings-body>label>span{font-size:13px}.settings-body input[type=text],select{width:100%;padding:8px 9px;border:1px solid var(--line);border-radius:7px;outline:0;background:var(--subtle);color:var(--ink)}.settings-body input:focus,select:focus{border-color:var(--accent)}
  .range{display:grid;grid-template-columns:1fr 65px;align-items:center;gap:10px}.range input{accent-color:var(--accent)}.range output{color:var(--muted);font-size:12px;text-align:right}
  footer{display:flex;justify-content:flex-end;gap:8px;padding:12px 16px;border-top:1px solid var(--line)}footer button{padding:8px 14px;border-radius:7px;cursor:pointer}.secondary{border:1px solid var(--line);background:transparent;color:var(--ink)}.primary{border:1px solid var(--accent);background:var(--accent);color:white}
</style>
