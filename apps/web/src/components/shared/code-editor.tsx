"use client";

import * as React from "react";
import CodeMirror, { type ReactCodeMirrorRef } from "@uiw/react-codemirror";
import { EditorView, keymap } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { tags } from "@lezer/highlight";
import { StreamLanguage, HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { indentSelection, undo, redo } from "@codemirror/commands";
import { openSearchPanel, search } from "@codemirror/search";
import { nginx } from "@codemirror/legacy-modes/mode/nginx";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { json } from "@codemirror/lang-json";
import { sql } from "@codemirror/lang-sql";
import { yaml } from "@codemirror/lang-yaml";
import { Search, WrapText, AlignLeft, Undo2, Redo2 } from "lucide-react";
import { useTheme } from "next-themes";
import { useT, useUI } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { guessLang, useCodePalette } from "./code-block";
import { editorSearchPanel } from "./editor-search-panel";

const apache = {
  token(stream: import("@codemirror/language").StringStream) {
    if (stream.eatSpace()) return null;
    if (stream.match(/^#.*/)) return "comment";
    if (stream.match(/^"(?:[^"\\]|\\.)*"/)) return "string";
    if (stream.match(/^[A-Za-z][A-Za-z0-9]*(?=\s|$)/)) return "keyword";
    if (stream.match(/^%\{[^}]+\}|^\$[0-9]+/)) return "variableName";
    stream.next(); return null;
  },
};

const redis = {
  token(stream: import("@codemirror/language").StringStream) {
    const start = stream.sol();
    if (stream.eatSpace()) return null;
    if (stream.match(/^#.*/)) return "comment";
    if (stream.match(/^"(?:[^"\\]|\\.)*"|^'(?:[^'\\]|\\.)*'/)) return "string";
    if (start && stream.match(/^[\w-]+/)) return "keyword";
    if (stream.match(/^(yes|no|on|off)\b/)) return "bool";
    if (stream.match(/^\d+(?:\.\d+)*(?:[kmg]b?)?\b/i)) return "number";
    stream.match(/^[^\s#]+/) || stream.next();
    return null;
  },
};

const hosts = {
  token(stream: import("@codemirror/language").StringStream) {
    if (stream.eatSpace()) return null;
    if (stream.match(/^#.*/)) return "comment";
    if (stream.match(/^(?:\d{1,3}\.){3}\d{1,3}(?=\s|$)|^[\da-f]*:[\da-f:.]*(?=\s|$)/i)) return "number";
    stream.match(/^[^\s#]+/) || stream.next();
    return "variableName";
  },
};

export type CodeEditorHandle = { jumpToLine: (line: number) => void };

/** Shared editable surface; search includes replace, case matching and regular expressions. */
export const CodeEditor = React.forwardRef<CodeEditorHandle, {
  value: string; onChange: (value: string) => void; label: string; language?: string;
  readOnly?: boolean; height?: string;
}>(function CodeEditor({ value, onChange, label, language, readOnly = false, height = "380px" }, ref) {
  const t = useT();
  const english = useUI((state) => state.lang === "en");
  const editor = React.useRef<ReactCodeMirrorRef>(null);
  const { resolvedTheme } = useTheme();
  const colors = useCodePalette();
  const [wrap, setWrap] = React.useState(false);
  const [error, setError] = React.useState("");
  const hint = language?.toLowerCase() ?? "";
  const lang = hint === "hosts" ? "hosts" : /(?:^|[/\\])redis(?:\.conf)?$/.test(hint) ? "redis" : hint === "caddy" || hint.endsWith("caddyfile") ? "caddy" : hint === "apache" ? "apache" : hint === "ini" ? "ini" : guessLang(language);
  const format = () => {
    const view = editor.current?.view;
    if (!view || readOnly) return;
    try {
      setError("");
      if (lang === "json") {
        view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: JSON.stringify(JSON.parse(view.state.doc.toString()), null, 2) + "\n" } });
      } else {
        // Language-aware indentation preserves comments, quoted values and YAML scalar text.
        const selection = view.state.selection;
        view.dispatch({ selection: { anchor: 0, head: view.state.doc.length } });
        indentSelection(view);
        view.dispatch({ selection: { anchor: Math.min(selection.main.anchor, view.state.doc.length), head: Math.min(selection.main.head, view.state.doc.length) } });
      }
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
  };
  const extensions = React.useMemo(() => [
    search({ top: true, createPanel: (view) => editorSearchPanel(view, english) }),
    syntaxHighlighting(HighlightStyle.define([
      { tag: tags.comment, color: "var(--code-muted)" },
      { tag: [tags.keyword, tags.operator, tags.bool, tags.null], color: "var(--code-keyword)" },
      { tag: [tags.string, tags.regexp], color: "var(--code-string)" },
      { tag: tags.number, color: "var(--code-number)" },
      { tag: [tags.propertyName, tags.attributeName, tags.tagName], color: "var(--code-key)" },
      { tag: [tags.variableName, tags.typeName], color: "var(--code-variable)" },
    ])),
    lang === "json" ? json() : lang === "yaml" ? yaml() : lang === "sql" ? sql() : StreamLanguage.define(lang === "hosts" ? hosts : lang === "redis" ? redis : lang === "apache" ? apache : lang === "nginx" ? nginx : lang === "shell" || lang === "caddy" ? shell : properties),
    EditorView.contentAttributes.of({ "aria-label": label }),
    EditorState.tabSize.of(2),
    ...(wrap ? [EditorView.lineWrapping] : []),
    EditorView.theme({
      "&": { backgroundColor: "var(--code-bg)", color: "var(--code-fg)", fontSize: "var(--code-font-size, 12px)" },
      ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.7" },
      ".cm-gutters": { backgroundColor: "var(--code-bg)", color: "var(--code-muted)", border: "none" },
      ".cm-cursor": { borderLeftColor: "var(--code-fg)" },
      ".cm-selectionBackground": { backgroundColor: "color-mix(in srgb, var(--code-key) 25%, transparent) !important" },
      ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "color-mix(in srgb, var(--code-fg) 6%, transparent)" },
      ".cm-content": { padding: "10px 0" },
      ".cm-panels": { backgroundColor: "var(--card)", color: "var(--foreground)", borderColor: "var(--border)" },
      ".nsb-editor-search": { padding: "12px 16px", display: "grid", gap: "10px", fontFamily: "var(--font-sans)", fontSize: "14px", lineHeight: "1.5" },
      ".nsb-search-header, .nsb-search-row, .nsb-search-actions, .nsb-search-options": { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap", minWidth: "0" },
      ".nsb-search-row[hidden]": { display: "none" },
      ".nsb-search-title": { fontWeight: "600" },
      ".nsb-search-status": { color: "var(--muted)", fontSize: "12px" },
      ".nsb-search-status.nsb-search-error": { color: "var(--error)" },
      ".nsb-search-input": { flex: "1 1 220px", minWidth: "0", width: "100%", height: "38px", border: "1px solid var(--border)", borderRadius: "10px", padding: "0 12px", background: "var(--fill)", color: "var(--foreground)", font: "inherit", outline: "none" },
      ".nsb-search-input:focus": { borderColor: "var(--primary)", background: "var(--card)" },
      ".nsb-search-input[aria-invalid=true]": { borderColor: "var(--error)" },
      ".nsb-search-input::placeholder": { color: "var(--muted)" },
      ".nsb-search-button": { height: "36px", padding: "0 14px", borderRadius: "18px", border: "1px solid transparent", background: "var(--fill)", color: "var(--foreground)", font: "inherit", fontWeight: "500", whiteSpace: "nowrap", cursor: "pointer", transition: "background-color 150ms, color 150ms" },
      ".nsb-search-button:hover:not(:disabled)": { background: "var(--card-2)" },
      ".nsb-search-button:focus-visible": { outline: "2px solid var(--primary)", outlineOffset: "2px" },
      ".nsb-search-button:disabled": { opacity: "0.45", cursor: "not-allowed" },
      ".nsb-search-toggle": { height: "32px", background: "transparent", color: "var(--secondary)", borderColor: "var(--border)", fontSize: "12px" },
      ".nsb-search-toggle[aria-pressed=true]": { background: "var(--primary-soft)", color: "var(--primary)", borderColor: "var(--primary)" },
      ".nsb-search-close": { marginLeft: "auto", width: "32px", height: "32px", padding: "0", fontSize: "22px", background: "transparent" },
      ".nsb-search-hint": { marginLeft: "auto", fontSize: "11px", color: "var(--muted)" },
      "&.cm-focused": { outline: "none" },
    }),
    keymap.of([{ key: "Mod-Shift-f", run: () => { format(); return true; } }]),
  ], [lang, label, wrap, readOnly, english]);
  React.useImperativeHandle(ref, () => ({ jumpToLine: (number) => {
    const view = editor.current?.view; if (!view) return;
    const line = view.state.doc.line(Math.max(1, Math.min(number, view.state.doc.lines)));
    view.dispatch({ selection: { anchor: line.from, head: line.to }, effects: EditorView.scrollIntoView(line.from, { y: "center" }) }); view.focus();
  } }));
  return <div className="min-w-0 overflow-hidden rounded-xl border border-border" style={colors}>
    <div className="flex flex-wrap items-center gap-1 border-b border-border bg-card px-2 py-1.5">
      <Button type="button" size="sm" variant="ghost" onClick={() => { if (editor.current?.view) openSearchPanel(editor.current.view); }}><Search className="h-3.5 w-3.5" />{t("editor.searchReplace")}</Button>
      <Button type="button" size="sm" variant="ghost" disabled={readOnly} onClick={format}><AlignLeft className="h-3.5 w-3.5" />{t(lang === "json" ? "editor.format" : "editor.indent")}</Button>
      <Button type="button" size="icon-sm" variant="ghost" aria-label={t("editor.undo")} disabled={readOnly} onClick={() => { if (editor.current?.view) undo(editor.current.view); }}><Undo2 className="h-3.5 w-3.5" /></Button>
      <Button type="button" size="icon-sm" variant="ghost" aria-label={t("editor.redo")} disabled={readOnly} onClick={() => { if (editor.current?.view) redo(editor.current.view); }}><Redo2 className="h-3.5 w-3.5" /></Button>
      <Button type="button" size="icon-sm" variant="ghost" aria-label={t("log.wrap")} aria-pressed={wrap} onClick={() => setWrap(!wrap)}><WrapText className="h-3.5 w-3.5" /></Button>
      <span className="ml-auto px-2 text-[10px] text-muted">{lang.toUpperCase()} · Ctrl F</span>
    </div>
    {error && <p role="alert" className="px-3 py-2 text-xs text-error">{error}</p>}
    <CodeMirror ref={editor} value={value} onChange={onChange} height={height} theme={resolvedTheme === "dark" ? "dark" : "light"} readOnly={readOnly} extensions={extensions} basicSetup={{ foldGutter: true, searchKeymap: true, highlightActiveLine: true }} />
  </div>;
});
