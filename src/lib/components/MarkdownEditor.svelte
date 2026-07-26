<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
  } from "@codemirror/commands";
  import { markdown } from "@codemirror/lang-markdown";
  import { languages } from "@codemirror/language-data";
  import {
    bracketMatching,
    defaultHighlightStyle,
    syntaxHighlighting,
  } from "@codemirror/language";
  import { openSearchPanel, searchKeymap } from "@codemirror/search";
  import { Compartment, EditorState, type Extension } from "@codemirror/state";
  import {
    crosshairCursor,
    drawSelection,
    dropCursor,
    EditorView,
    highlightActiveLine,
    highlightActiveLineGutter,
    highlightSpecialChars,
    keymap,
    lineNumbers,
    rectangularSelection,
  } from "@codemirror/view";
  import type { SettingsV1 } from "../api/types";
  import { translate, type Locale } from "../i18n";
  import { formatSelection, replaceCurrentLine, type FormatName } from "../editor/commands";
  import { fusionExtension } from "../editor/fusion";
  import {
    cacheEditorState,
    createCachedEditorState,
    rebaseEditorState,
    type EditorHistoryRewrite,
  } from "../editor/state-cache";

  export let value: string;
  export let locale: Locale = "zh-CN";
  export let documentId: string;
  export let documentVersion = 0;
  export let mode: "live" | "source" = "live";
  export let readOnly = false;
  export let allowRemoteImages = false;
  export let settings: SettingsV1;
  export let onChange: (value: string) => void = () => undefined;
  export let onPasteImage: (documentId: string, file: File, placeholder: string) => Promise<void> = async () => undefined;
  export let loadResource: (documentId: string, source: string) => Promise<string>;
  export let cachedState: unknown = null;
  export let historyRewrite: EditorHistoryRewrite | undefined;
  export let onStateChange: (documentId: string, state: unknown) => void = () => undefined;
  export let onHistoryRewriteApplied: (documentId: string, documentVersion: number) => void = () => undefined;

  let host: HTMLDivElement;
  let wrapper: HTMLDivElement;
  let view: EditorView | null = null;
  let selectionOpen = false;
  let selectionX = 0;
  let selectionY = 0;
  let slashOpen = false;
  let slashX = 0;
  let slashY = 0;
  let applyingExternal = false;
  let baseExtensions: Extension[] = [];
  let lastKnownValue = value;
  let currentModeKey = "";
  let currentThemeKey = "";
  let currentReadOnly = readOnly;
  const modeCompartment = new Compartment();
  const themeCompartment = new Compartment();
  const editableCompartment = new Compartment();

  $: slashCommands = [
    { label: translate(locale, "heading1"), hint: "#", prefix: "# " },
    { label: translate(locale, "heading2"), hint: "##", prefix: "## " },
    { label: translate(locale, "heading3"), hint: "###", prefix: "### " },
    { label: translate(locale, "bulletList"), hint: "•", prefix: "- " },
    { label: translate(locale, "task"), hint: "☐", prefix: "- [ ] " },
    { label: translate(locale, "quote"), hint: "❯", prefix: "> " },
    { label: translate(locale, "codeBlock"), hint: "</>", prefix: "```\n\n```" },
    { label: translate(locale, "mathBlock"), hint: "∑", prefix: "$$\n\n$$" },
  ];

  onMount(() => {
    const updateListener = EditorView.updateListener.of((update) => {
      if (update.docChanged && !applyingExternal) {
        const content = update.state.doc.toString();
        lastKnownValue = content;
        onChange(content);
      }
      if (update.selectionSet || update.docChanged) {
        updateFloatingUi(update.view);
        if (settings.typewriterMode && update.selectionSet) {
          requestAnimationFrame(() => {
            if (view) view.dispatch({ effects: EditorView.scrollIntoView(view.state.selection.main.head, { y: "center" }) });
          });
        }
      }
    });

    function insertImage(editor: EditorView, image: File, from: number, to: number): void {
      if (editor.state.readOnly) return;
      const alt = image.name.replace(/\.[^.]+$/, "");
      const placeholder = `![${alt}](inkflow-upload://${crypto.randomUUID()})`;
      editor.dispatch({
        changes: { from, to, insert: placeholder },
        selection: { anchor: from + placeholder.length },
        userEvent: "input.paste.image",
      });
      void onPasteImage(documentId, image, placeholder);
    }

    const pasteHandler = EditorView.domEventHandlers({
      paste(event, editor) {
        if (editor.state.readOnly) return false;
        const image = Array.from(event.clipboardData?.files ?? []).find((file) => file.type.startsWith("image/"));
        if (!image) return false;
        event.preventDefault();
        const selection = editor.state.selection.main;
        insertImage(editor, image, selection.from, selection.to);
        return true;
      },
      drop(event, editor) {
        if (editor.state.readOnly) return false;
        const image = Array.from(event.dataTransfer?.files ?? []).find((file) => file.type.startsWith("image/"));
        if (!image) return false;
        const position = editor.posAtCoords({ x: event.clientX, y: event.clientY });
        if (position === null) return false;
        event.preventDefault();
        insertImage(editor, image, position, position);
        return true;
      },
    });

    const customKeys = keymap.of([
      { key: "Mod-b", run: (editor) => formatSelection(editor, "bold") },
      { key: "Mod-i", run: (editor) => formatSelection(editor, "italic") },
      { key: "Mod-Shift-x", run: (editor) => formatSelection(editor, "strike") },
      { key: "Mod-k", run: (editor) => formatSelection(editor, "link") },
      indentWithTab,
      ...closeBracketsKeymap,
      ...defaultKeymap,
      ...historyKeymap,
      ...searchKeymap,
    ]);

    baseExtensions = [
        highlightSpecialChars(),
        history(),
        drawSelection(),
        dropCursor(),
        rectangularSelection(),
        crosshairCursor(),
        highlightActiveLine(),
        markdown({ codeLanguages: languages }),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        bracketMatching(),
        closeBrackets(),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({
          "aria-label": "Markdown editor",
          spellcheck: "true",
          autocapitalize: "sentences",
        }),
        customKeys,
        pasteHandler,
        updateListener,
      ];
    const state = createCachedEditorState(
      value,
      extensionsForCurrentState(),
      cachedState,
      documentVersion,
    );
    if (cachedState !== null) onStateChange(documentId, null);
    view = new EditorView({ state, parent: host });
    lastKnownValue = value;
    currentModeKey = modeKey(mode, documentId, allowRemoteImages);
    currentThemeKey = themeKey(settings, mode);
    view.focus();
  });

  onDestroy(() => {
    if (!view) return;
    onStateChange(documentId, cacheEditorState(view.state, documentVersion));
    view.destroy();
  });

  $: if (view && value !== lastKnownValue) {
    applyingExternal = true;
    const pendingRewrite = historyRewrite;
    if (
      pendingRewrite
      && pendingRewrite.previousDoc === lastKnownValue
      && pendingRewrite.nextDoc === value
    ) {
      view.setState(rebaseEditorState(
        view.state,
        value,
        extensionsForCurrentState(),
        pendingRewrite.edits,
      ));
    } else {
      const selection = view.state.selection.main;
      view.setState(EditorState.create({
        doc: value,
        selection: {
          anchor: Math.min(selection.anchor, value.length),
          head: Math.min(selection.head, value.length),
        },
        extensions: extensionsForCurrentState(),
      }));
    }
    lastKnownValue = value;
    applyingExternal = false;
    if (pendingRewrite) {
      onHistoryRewriteApplied(documentId, pendingRewrite.documentVersion);
    }
  }

  $: if (view && modeKey(mode, documentId, allowRemoteImages) !== currentModeKey) {
    currentModeKey = modeKey(mode, documentId, allowRemoteImages);
    view.dispatch({
      effects: [
        modeCompartment.reconfigure(modeExtensions(mode)),
        themeCompartment.reconfigure(editorTheme(settings, mode)),
      ],
    });
  }

  $: if (view && themeKey(settings, mode) !== currentThemeKey) {
    currentThemeKey = themeKey(settings, mode);
    view.dispatch({ effects: themeCompartment.reconfigure(editorTheme(settings, mode)) });
  }

  $: if (view && readOnly !== currentReadOnly) {
    currentReadOnly = readOnly;
    view.dispatch({ effects: editableCompartment.reconfigure(editableExtensions(readOnly)) });
  }

  function modeExtensions(nextMode: "live" | "source"): Extension {
    if (nextMode === "source") {
      return [lineNumbers(), highlightActiveLineGutter()];
    }
    return fusionExtension({ documentId, loadResource, allowRemoteImages });
  }

  function editableExtensions(disabled: boolean): Extension {
    return [EditorState.readOnly.of(disabled), EditorView.editable.of(!disabled)];
  }

  function extensionsForCurrentState(): Extension[] {
    return [
      baseExtensions,
      modeCompartment.of(modeExtensions(mode)),
      themeCompartment.of(editorTheme(settings, mode)),
      editableCompartment.of(editableExtensions(readOnly)),
    ];
  }

  function editorTheme(current: SettingsV1, nextMode: string): Extension {
    return EditorView.theme({
      "&": {
        height: "100%",
        backgroundColor: "transparent",
        color: "var(--ink)",
        fontSize: `${current.fontSize}px`,
      },
      ".cm-scroller": {
        fontFamily: nextMode === "source" ? current.codeFont : current.editorFont,
        lineHeight: String(current.lineHeight),
        overflow: "auto",
      },
      ".cm-content": {
        width: "100%",
        maxWidth: `${current.pageWidth}px`,
        minHeight: "calc(100vh - 126px)",
        margin: "0 auto",
        padding: "56px 48px 35vh",
        caretColor: "var(--accent)",
      },
      ".cm-focused": { outline: "none" },
      ".cm-cursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
      ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
        backgroundColor: "var(--selection)",
      },
      ".cm-activeLine": { backgroundColor: "transparent" },
      ".cm-gutters": {
        backgroundColor: "transparent",
        border: "none",
        color: "var(--muted-2)",
        paddingLeft: "8px",
      },
      ".cm-line": { padding: "0" },
    });
  }

  function themeKey(current: SettingsV1, nextMode: string): string {
    return [current.pageWidth, current.fontSize, current.lineHeight, current.editorFont, current.codeFont, nextMode].join("|");
  }

  function modeKey(nextMode: string, nextDocumentId: string, remoteImages: boolean): string {
    return `${nextMode}|${nextDocumentId}|${remoteImages}`;
  }

  function updateFloatingUi(editor: EditorView): void {
    const selection = editor.state.selection.main;
    selectionOpen = !selection.empty && !readOnly;
    if (selectionOpen && wrapper) {
      const coords = editor.coordsAtPos(selection.head);
      const bounds = wrapper.getBoundingClientRect();
      if (coords) {
        selectionX = Math.max(150, Math.min(bounds.width - 150, coords.left - bounds.left));
        selectionY = Math.max(48, coords.top - bounds.top - 48);
      }
    }
    const line = editor.state.doc.lineAt(selection.head);
    slashOpen = !readOnly && line.text.trim() === "/";
    if (slashOpen && wrapper) {
      const coords = editor.coordsAtPos(selection.head);
      const bounds = wrapper.getBoundingClientRect();
      if (coords) {
        slashX = Math.max(12, Math.min(bounds.width - 270, coords.left - bounds.left));
        slashY = Math.min(bounds.height - 330, coords.bottom - bounds.top + 8);
      }
    }
  }

  function applyFormat(format: FormatName): void {
    if (!view) return;
    formatSelection(view, format);
    selectionOpen = false;
    view.focus();
  }

  function applySlash(prefix: string): void {
    if (!view) return;
    replaceCurrentLine(view, prefix);
    slashOpen = false;
  }

  export function focus(): void {
    view?.focus();
  }

  export function runFormat(format: FormatName): void {
    if (view) formatSelection(view, format);
  }

  export function openFind(): void {
    if (view) openSearchPanel(view);
  }

  export function goToLine(lineNumber: number): void {
    if (!view) return;
    const line = view.state.doc.line(Math.min(Math.max(lineNumber, 1), view.state.doc.lines));
    view.dispatch({ selection: { anchor: line.from }, effects: EditorView.scrollIntoView(line.from, { y: "center" }) });
    view.focus();
  }
</script>

<div class="editor-wrapper" bind:this={wrapper}>
  <div class="editor-host" bind:this={host}></div>

  {#if selectionOpen}
    <div class="selection-toolbar" style={`left:${selectionX}px;top:${selectionY}px`} role="toolbar" aria-label="Text formatting">
      <button title="Bold (Ctrl+B)" on:mousedown|preventDefault={() => applyFormat("bold")}><strong>B</strong></button>
      <button title="Italic (Ctrl+I)" on:mousedown|preventDefault={() => applyFormat("italic")}><em>I</em></button>
      <button title="Strikethrough" on:mousedown|preventDefault={() => applyFormat("strike")}><s>S</s></button>
      <button title="Inline code" on:mousedown|preventDefault={() => applyFormat("code")}>&lt;/&gt;</button>
      <button title="Link (Ctrl+K)" on:mousedown|preventDefault={() => applyFormat("link")}>↗</button>
    </div>
  {/if}

  {#if slashOpen}
    <div class="slash-menu" style={`left:${slashX}px;top:${slashY}px`} role="menu">
      <div class="slash-title">{translate(locale, "insertContent")}</div>
      {#each slashCommands as command}
        <button role="menuitem" on:mousedown|preventDefault={() => applySlash(command.prefix)}>
          <span>{command.hint}</span><span>{command.label}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .editor-wrapper,.editor-host{position:relative;width:100%;height:100%;min-width:0}
  .selection-toolbar{position:absolute;z-index:30;display:flex;transform:translateX(-50%);padding:4px;border:1px solid var(--line);border-radius:9px;background:var(--panel);box-shadow:var(--shadow-lg)}
  .selection-toolbar button{width:34px;height:30px;border:0;border-radius:6px;background:transparent;color:var(--ink);font:14px/1 var(--ui-font);cursor:pointer}
  .selection-toolbar button:hover{background:var(--hover)}
  .slash-menu{position:absolute;z-index:35;width:250px;padding:6px;border:1px solid var(--line);border-radius:10px;background:var(--panel);box-shadow:var(--shadow-lg)}
  .slash-title{padding:7px 10px 5px;color:var(--muted);font-size:11px;font-weight:650;text-transform:uppercase;letter-spacing:.08em}
  .slash-menu button{display:grid;grid-template-columns:34px 1fr;align-items:center;width:100%;padding:7px 8px;border:0;border-radius:7px;background:transparent;color:var(--ink);text-align:left;cursor:pointer}
  .slash-menu button:hover{background:var(--hover)}
  .slash-menu button span:first-child{color:var(--muted);font-family:var(--code-font)}
</style>
