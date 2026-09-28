"use client";

import * as React from "react";
import CodeMirror, { type ReactCodeMirrorRef } from "@uiw/react-codemirror";
import { EditorView, keymap } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { tags } from "@lezer/highlight";
import { StreamLanguage, HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { indentSelection, undo, redo } from "@codemirror/commands";
import { openSearchPanel } from "@codemirror/search";
import { nginx } from "@codemirror/legacy-modes/mode/nginx";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { json } from "@codemirror/lang-json";
import { sql } from "@codemirror/lang-sql";
import { yaml } from "@codemirror/lang-yaml";
import { Search, WrapText, AlignLeft, Undo2, Redo2 } from "lucide-react";
import { useTheme } from "next-themes";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { guessLang, useCodePalette } from "./code-block";

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

export type CodeEditorHandle = { jumpToLine: (line: number) => void };

/** Shared editable surface; search includes replace, case matching and regular expressions. */
export const CodeEditor = React.forwardRef<CodeEditorHandle, {
  value: string; onChange: (value: string) => void; label: string; language?: string;
  readOnly?: boolean; height?: string;
}>(function CodeEditor({ value, onChange, label, language, readOnly = false, height = "380px" }, ref) {
  const t = useT();
  const editor = React.useRef<ReactCodeMirrorRef>(null);
  const { resolvedTheme } = useTheme();
  const colors = useCodePalette();
  const [wrap, setWrap] = React.useState(false);
  const [error, setError] = React.useState("");
  const lang = language === "apache" ? "apache" : language === "ini" ? "ini" : guessLang(language);
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
    syntaxHighlighting(HighlightStyle.define([
      { tag: tags.comment, color: "var(--code-muted)" },
      { tag: [tags.keyword, tags.operator, tags.bool, tags.null], color: "var(--code-keyword)" },
      { tag: [tags.string, tags.regexp], color: "var(--code-string)" },
      { tag: tags.number, color: "var(--code-number)" },
      { tag: [tags.propertyName, tags.attributeName, tags.tagName], color: "var(--code-key)" },
      { tag: [tags.variableName, tags.typeName], color: "var(--code-variable)" },
    ])),
    lang === "json" ? json() : lang === "yaml" ? yaml() : lang === "sql" ? sql() : StreamLanguage.define(lang === "apache" ? apache : lang === "nginx" ? nginx : lang === "shell" ? shell : properties),
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
      ".cm-panels": { backgroundColor: "var(--card)", color: "var(--foreground)" },
      ".cm-search": { display: "flex", flexWrap: "wrap", gap: "6px", padding: "10px" },
      ".cm-textfield": { backgroundColor: "var(--card)", color: "var(--foreground)", borderRadius: "6px" },
      ".cm-button": { backgroundImage: "none", backgroundColor: "var(--fill)", color: "var(--foreground)", borderRadius: "6px" },
      "&.cm-focused": { outline: "none" },
    }),
    keymap.of([{ key: "Mod-Shift-f", run: () => { format(); return true; } }]),
  ], [lang, label, wrap, readOnly]);
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
