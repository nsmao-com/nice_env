"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ChevronRight, FileText, Folder, Loader2, RefreshCw, Save } from "lucide-react";
import { useT } from "@/lib/store";
import { isTauri, normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { fmtBytes } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/shared/misc";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";

type EditorState = { path: string; content: string; original: string; revision: string };

function joinPath(root: string, relative: string) {
  if (!relative) return root;
  return `${root.replace(/[\\/]$/, "")}/${relative}`;
}

export function SiteFileBrowser({ siteId, active, disabled, onBusyChange }: {
  siteId: string;
  active: boolean;
  disabled?: boolean;
  onBusyChange?: (busy: boolean) => void;
}) {
  const t = useT();
  const client = useQueryClient();
  const [current, setCurrent] = React.useState("");
  const [editor, setEditor] = React.useState<EditorState | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [discardOpen, setDiscardOpen] = React.useState(false);
  const [saved, setSaved] = React.useState(false);
  const busyRef = React.useRef(false);
  const editorOpenRef = React.useRef(false);
  const openerRef = React.useRef<HTMLButtonElement | null>(null);
  const textareaRef = React.useRef<HTMLTextAreaElement | null>(null);
  const dirty = !!editor && editor.content !== editor.original;
  const locked = busy || disabled;
  const directory = useQuery({
    queryKey: ["site-directory", siteId, current],
    queryFn: () => api.siteDirectory(siteId, current),
    enabled: active && !disabled,
    retry: false,
    staleTime: 0,
  });

  const setWorking = (value: boolean) => {
    busyRef.current = value;
    setBusy(value);
    onBusyChange?.(value || editorOpenRef.current);
  };

  React.useEffect(() => {
    if (!active && !editorOpenRef.current && !busyRef.current) {
      setCurrent("");
      setEditor(null);
      setError(null);
    }
  }, [active, siteId]);

  const openEntry = async (entry: api.SiteFileEntry, opener: HTMLButtonElement) => {
    if (busyRef.current || disabled) return;
    if (entry.directory) {
      setCurrent(entry.path);
      setError(null);
      return;
    }
    setWorking(true);
    setError(null);
    setSaved(false);
    openerRef.current = opener;
    try {
      const file = await api.siteFileRead(siteId, entry.path);
      editorOpenRef.current = true;
      setEditor({ path: file.path, content: file.content, original: file.content, revision: file.revision });
    } catch (failure) {
      setError([normalizeError(failure).message, normalizeError(failure).hint].filter(Boolean).join(" · "));
    } finally {
      setWorking(false);
    }
  };

  const save = async () => {
    if (!editor || busyRef.current || disabled || !dirty) return;
    setWorking(true);
    setError(null);
    try {
      const saved = await api.siteFileWrite(siteId, editor.path, editor.content, editor.revision);
      setEditor({ path: saved.path, content: saved.content, original: saved.content, revision: saved.revision });
      setSaved(true);
      await client.invalidateQueries({ queryKey: ["site-directory", siteId, current] });
    } catch (failure) {
      setError([normalizeError(failure).message, normalizeError(failure).hint].filter(Boolean).join(" · "));
    } finally {
      setWorking(false);
    }
  };

  const closeEditor = () => {
    if (busyRef.current) return;
    editorOpenRef.current = false;
    setEditor(null);
    setError(null);
    setSaved(false);
    setDiscardOpen(false);
    onBusyChange?.(false);
  };

  const requestClose = () => {
    if (busyRef.current) return;
    if (dirty) setDiscardOpen(true);
    else closeEditor();
  };

  return <section className="min-w-0 space-y-3 rounded-xl border border-border p-3 sm:p-4">
    <div className="flex flex-wrap items-start justify-between gap-2">
      <div className="min-w-0">
        <h3 className="text-sm font-semibold">{t("siteFiles.browserTitle" as never)}</h3>
        <p className="mt-1 text-xs leading-relaxed text-muted">{t("siteFiles.browserHint" as never)}</p>
      </div>
      <Button size="sm" variant="ghost" disabled={locked || directory.isFetching} onClick={() => void directory.refetch()}>
        <RefreshCw className={directory.isFetching ? "size-3.5 animate-spin" : "size-3.5"} />{t("siteFiles.refresh")}
      </Button>
    </div>
    {!isTauri && <p className="flex items-start gap-2 rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn"><AlertTriangle className="mt-0.5 size-3.5 shrink-0" />{t("siteFiles.browserPreview" as never)}</p>}
    {disabled && <p className="text-xs text-warn">{t("siteFiles.browserUnavailable")}</p>}
    <>
      <div className="flex min-w-0 items-center gap-1 overflow-x-auto rounded-lg bg-fill px-2.5 py-2 text-xs">
        <button type="button" className="shrink-0 rounded px-1.5 py-1 font-medium hover:bg-card-2" disabled={locked || !current} onClick={() => setCurrent("")}>{t("siteFiles.browserRoot" as never)}</button>
        {current.split("/").filter(Boolean).map((part, index, parts) => {
          const path = parts.slice(0, index + 1).join("/");
          return <React.Fragment key={path}><ChevronRight className="size-3 shrink-0 text-faint" /><button type="button" className="shrink-0 rounded px-1.5 py-1 font-mono hover:bg-card-2" disabled={locked} onClick={() => setCurrent(path)}>{part}</button></React.Fragment>;
        })}
      </div>
      {directory.error ? <div role="alert" className="space-y-2 rounded-lg bg-error-soft p-3 text-xs text-error"><p>{normalizeError(directory.error).message}</p><Button size="sm" variant="secondary" disabled={locked || directory.isFetching} onClick={() => void directory.refetch()}>{t("siteFiles.retry")}</Button></div> : directory.isLoading ? <p role="status" className="flex items-center gap-2 py-5 text-xs text-muted"><Loader2 className="size-3.5 animate-spin" />{t("common.loading")}</p> : directory.data && <>
        <div className="flex items-center justify-between gap-2 text-[11px] text-faint"><span className="min-w-0 truncate font-mono" title={joinPath(directory.data.root, directory.data.current)}>{joinPath(directory.data.root, directory.data.current)}</span><span className="shrink-0">{directory.data.entries.length}</span></div>
        <div className="max-h-64 overflow-y-auto rounded-lg border border-border">
          {directory.data.parent != null && <button type="button" className="flex w-full items-center gap-2 border-b border-dashed border-separator px-3 py-2.5 text-left text-xs text-muted hover:bg-fill" disabled={locked} onClick={() => setCurrent(directory.data.parent ?? "")}><Folder className="size-3.5 shrink-0" />{t("siteFiles.browserParent")}</button>}
          {directory.data.entries.map((entry) => <button key={entry.path} type="button" className="flex w-full min-w-0 items-center gap-2 border-b border-dashed border-separator px-3 py-2.5 text-left text-xs last:border-b-0 hover:bg-fill disabled:opacity-60" disabled={locked} onClick={(event) => void openEntry(entry, event.currentTarget)}>
            {entry.directory ? <Folder className="size-3.5 shrink-0 text-primary" /> : <FileText className="size-3.5 shrink-0 text-faint" />}
            <span className="min-w-0 flex-1 truncate font-mono">{entry.name}</span>
            <span className="shrink-0 text-[10px] text-faint">{entry.directory ? t("siteFiles.browserFolder" as never) : fmtBytes(entry.sizeBytes)}</span>
          </button>)}
          {!directory.data.entries.length && <p className="p-4 text-center text-xs text-muted">{t("siteFiles.browserEmpty" as never)}</p>}
        </div>
      </>}
    </>
    {error && !editor && <p role="alert" className="text-xs leading-relaxed text-error [overflow-wrap:anywhere]">{error}</p>}
    <Dialog open={!!editor} onOpenChange={(open) => { if (!open) requestClose(); }}>
      <DialogContent hideClose={busy} onCloseAutoFocus={(event) => { event.preventDefault(); openerRef.current?.focus(); }} className="flex max-h-[90dvh] max-w-3xl flex-col gap-0 overflow-y-auto p-0">
        <DialogHeader className="shrink-0 border-b border-dashed border-separator px-4 py-4 sm:px-5">
          <DialogTitle className="flex min-w-0 items-center gap-2 pr-8 text-sm leading-relaxed"><FileText className="size-4 shrink-0 text-primary" /><span className="min-w-0 [overflow-wrap:anywhere]">{editor?.path}</span></DialogTitle>
          <DialogDescription className="text-xs">{t("siteFiles.browserEditHint" as never)}</DialogDescription>
        </DialogHeader>
        <textarea ref={textareaRef} aria-label={editor?.path ?? t("siteFiles.browserTitle")} value={editor?.content ?? ""} disabled={locked} spellCheck={false} onChange={(event) => { setSaved(false); setEditor((value) => value ? { ...value, content: event.target.value } : value); }} className="h-[min(50dvh,32rem)] min-h-24 w-full min-w-0 shrink-0 resize-none bg-card px-4 py-3 font-mono text-xs leading-5 text-foreground outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary sm:px-5" />
        {error && <p role="alert" className="shrink-0 px-4 py-2 text-xs leading-relaxed text-error [overflow-wrap:anywhere] sm:px-5">{error}</p>}
        <DialogFooter className="sticky bottom-0 shrink-0 border-t border-dashed border-separator bg-card px-4 py-3 sm:px-5">
          <span role="status" className="mr-auto text-xs text-muted">{dirty ? t("detail.unsaved") : saved ? t("siteFiles.browserSaved") : ""}</span>
          <Button variant="ghost" disabled={busy} onClick={requestClose}>{t("common.close")}</Button>
          <Button disabled={locked || !dirty} onClick={() => void save()}>{busy && <Loader2 className="size-3.5 animate-spin" />}<Save className="size-3.5" />{t("siteFiles.browserSave" as never)}</Button>
        </DialogFooter>
        <ConfirmDialog open={discardOpen} onOpenChange={setDiscardOpen} title={t("detail.discardTitle")} description={t("siteFiles.browserDiscard")} confirmText={t("detail.discard")} danger onConfirm={closeEditor} onCloseAutoFocus={(event) => { event.preventDefault(); if (editorOpenRef.current) textareaRef.current?.focus(); else openerRef.current?.focus(); }} />
      </DialogContent>
    </Dialog>
  </section>;
}
