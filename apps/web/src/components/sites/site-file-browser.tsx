"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { AlertTriangle, ChevronRight, Download, FilePlus, FileText, Folder, FolderPlus, Loader2, Move, Pencil, RefreshCw, Save, Trash2, Upload } from "lucide-react";
import { useT } from "@/lib/store";
import { isTauri, normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { fmtBytes } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
  const [deleteEntry, setDeleteEntry] = React.useState<api.SiteFileEntry | null>(null);
  const [deleteConfirmation, setDeleteConfirmation] = React.useState("");
  const [deleteError, setDeleteError] = React.useState<string | null>(null);
  const [createKind, setCreateKind] = React.useState<"file" | "directory" | null>(null);
  const [createName, setCreateName] = React.useState("");
  const [createError, setCreateError] = React.useState<string | null>(null);
  const [renameEntry, setRenameEntry] = React.useState<api.SiteFileEntry | null>(null);
  const [renameName, setRenameName] = React.useState("");
  const [renameError, setRenameError] = React.useState<string | null>(null);
  const [moveEntry, setMoveEntry] = React.useState<api.SiteFileEntry | null>(null);
  const [movePath, setMovePath] = React.useState("");
  const [moveError, setMoveError] = React.useState<string | null>(null);
  const busyRef = React.useRef(false);
  const editorOpenRef = React.useRef(false);
  const openerRef = React.useRef<HTMLButtonElement | null>(null);
  const moveOpenerRef = React.useRef<HTMLButtonElement | null>(null);
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

  const relativeNameValid = (name: string) => {
    const trimmed = name.trim();
    return trimmed.length > 0 && trimmed.length <= 255 && !/[\\/:\u0000-\u001f\u007f-\u009f]/.test(trimmed) && trimmed !== "." && trimmed !== "..";
  };

  const relativePathValid = (path: string) => {
    const normalized = path.trim().replaceAll("\\", "/");
    if (!normalized || normalized.length > 2048 || normalized.startsWith("/") || normalized.endsWith("/")) return false;
    return normalized.split("/").every((part) => relativeNameValid(part));
  };

  const createEntry = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!createKind || !relativeNameValid(createName) || busyRef.current || disabled) return;
    const name = createName.trim();
    const path = current ? `${current}/${name}` : name;
    setWorking(true); setCreateError(null);
    try {
      await api.siteFileCreate(siteId, path, createKind === "directory");
      setCreateKind(null); setCreateName("");
      await client.invalidateQueries({ queryKey: ["site-directory", siteId] });
      toast.success(t(createKind === "directory" ? "siteFiles.browserCreatedDirectory" as never : "siteFiles.browserCreatedFile" as never));
    } catch (failure) {
      const parsed = normalizeError(failure); setCreateError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
    } finally { setWorking(false); }
  };

  const uploadFile = async () => {
    if (busyRef.current || disabled) return;
    setWorking(true); setError(null);
    try {
      let source: string | null = "C:/Demo/uploads/readme.md";
      if (isTauri) {
        const { open } = await import("@tauri-apps/plugin-dialog");
        const picked = await open({ directory: false, multiple: false, title: t("siteFiles.browserUpload" as never) });
        source = typeof picked === "string" ? picked : null;
      }
      if (!source) return;
      const fileName = source.replaceAll("\\", "/").split("/").pop() ?? "";
      if (!relativeNameValid(fileName)) throw { code: "SITE_FILE_UPLOAD_INVALID", message: t("siteFiles.browserUploadInvalid" as never) };
      const path = current ? `${current}/${fileName}` : fileName;
      await api.siteFileUpload(siteId, source, path);
      await client.invalidateQueries({ queryKey: ["site-directory", siteId] });
      toast.success(t("siteFiles.browserUploaded" as never));
    } catch (failure) {
      const parsed = normalizeError(failure); setError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
    } finally { setWorking(false); }
  };

  const downloadFile = async (entry: api.SiteFileEntry) => {
    if (entry.directory || busyRef.current || disabled) return;
    setWorking(true); setError(null);
    try {
      if (isTauri) {
        const { save } = await import("@tauri-apps/plugin-dialog");
        const destination = await save({
          title: t("siteFiles.browserDownload" as never),
          defaultPath: entry.name,
        });
        if (!destination) return;
        await api.siteFileDownload(siteId, entry.path, destination);
      } else {
        // 浏览器预览没有本机文件路径，读取当前示例文本并生成真实下载文件。
        const file = await api.siteFileRead(siteId, entry.path);
        const url = URL.createObjectURL(new Blob([file.content], { type: "text/plain;charset=utf-8" }));
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = entry.name;
        anchor.click();
        window.setTimeout(() => URL.revokeObjectURL(url), 0);
      }
      toast.success(t("siteFiles.browserDownloaded" as never));
    } catch (failure) {
      const parsed = normalizeError(failure);
      setError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
    } finally { setWorking(false); }
  };

  const renameEntryNow = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!renameEntry || !relativeNameValid(renameName) || busyRef.current || disabled) return;
    const name = renameName.trim();
    const parent = renameEntry.path.split("/").slice(0, -1).join("/");
    const newPath = parent ? `${parent}/${name}` : name;
    setWorking(true); setRenameError(null);
    try {
      await api.siteFileRename(siteId, renameEntry.path, newPath);
      if (editor?.path === renameEntry.path) {
        editorOpenRef.current = false; setEditor(null); setSaved(false);
      }
      setRenameEntry(null); setRenameName("");
      await client.invalidateQueries({ queryKey: ["site-directory", siteId] });
      toast.success(t("siteFiles.browserRenamed" as never));
    } catch (failure) {
      const parsed = normalizeError(failure); setRenameError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
    } finally { setWorking(false); }
  };

  const moveEntryNow = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!moveEntry || !relativePathValid(movePath) || busyRef.current || disabled) return;
    const target = movePath.trim().replaceAll("\\", "/");
    if (target === moveEntry.path) return;
    setWorking(true); setMoveError(null);
    try {
      await api.siteFileRename(siteId, moveEntry.path, target);
      if (editor?.path === moveEntry.path) {
        editorOpenRef.current = false; setEditor(null); setSaved(false);
      }
      setMoveEntry(null); setMovePath("");
      await client.invalidateQueries({ queryKey: ["site-directory", siteId] });
      toast.success(t("siteFiles.browserMoved" as never));
    } catch (failure) {
      const parsed = normalizeError(failure); setMoveError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
    } finally { setWorking(false); }
  };

  const deleteEntryNow = async () => {
    if (!deleteEntry || busyRef.current || disabled || deleteConfirmation !== deleteEntry.path) return;
    setWorking(true);
    setDeleteError(null);
    try {
      await api.siteFileDelete(siteId, deleteEntry.path, deleteConfirmation);
      toast.success(t("siteFiles.browserDeleted" as never));
      setDeleteEntry(null);
      setDeleteConfirmation("");
      await client.invalidateQueries({ queryKey: ["site-directory", siteId, current] });
    } catch (failure) {
      const parsed = normalizeError(failure);
      setDeleteError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
    } finally {
      setWorking(false);
    }
  };

  return <section className="min-w-0 space-y-3 rounded-xl border border-border p-3 sm:p-4">
    <div className="flex flex-wrap items-start justify-between gap-2">
      <div className="min-w-0">
        <h3 className="text-sm font-semibold">{t("siteFiles.browserTitle" as never)}</h3>
        <p className="mt-1 text-xs leading-relaxed text-muted">{t("siteFiles.browserHint" as never)}</p>
      </div>
      <div className="flex flex-wrap items-center justify-end gap-1.5">
        <Button size="sm" variant="secondary" disabled={locked || directory.isFetching} onClick={() => void uploadFile()}><Upload className="size-3.5" />{t("siteFiles.browserUpload" as never)}</Button>
        <Button size="sm" variant="secondary" disabled={locked || directory.isFetching} onClick={() => { setCreateError(null); setCreateName(""); setCreateKind("file"); }}><FilePlus className="size-3.5" />{t("siteFiles.browserNewFile" as never)}</Button>
        <Button size="sm" variant="secondary" disabled={locked || directory.isFetching} onClick={() => { setCreateError(null); setCreateName(""); setCreateKind("directory"); }}><FolderPlus className="size-3.5" />{t("siteFiles.browserNewFolder" as never)}</Button>
        <Button size="sm" variant="ghost" disabled={locked || directory.isFetching} onClick={() => void directory.refetch()}>
          <RefreshCw className={directory.isFetching ? "size-3.5 animate-spin" : "size-3.5"} />{t("siteFiles.refresh")}
        </Button>
      </div>
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
           {directory.data.entries.map((entry) => <div key={entry.path} className="flex w-full min-w-0 items-center gap-2 border-b border-dashed border-separator px-3 py-1.5 last:border-b-0 hover:bg-fill">
             <button type="button" className="flex min-w-0 flex-1 items-center gap-2 py-1 text-left text-xs disabled:opacity-60" disabled={locked} onClick={(event) => void openEntry(entry, event.currentTarget)}>
               {entry.directory ? <Folder className="size-3.5 shrink-0 text-primary" /> : <FileText className="size-3.5 shrink-0 text-faint" />}
               <span className="min-w-0 flex-1 truncate font-mono">{entry.name}</span>
               <span className="shrink-0 text-[10px] text-faint">{entry.directory ? t("siteFiles.browserFolder" as never) : fmtBytes(entry.sizeBytes)}</span>
             </button>
             <div className="flex shrink-0 flex-wrap items-center justify-end gap-0.5">
               {!entry.directory && <Button type="button" size="icon" variant="ghost" className="h-7 w-7 shrink-0" disabled={locked} aria-label={`${t("siteFiles.browserDownload" as never)} ${entry.name}`} title={t("siteFiles.browserDownload" as never)} onClick={() => void downloadFile(entry)}><Download className="size-3.5" /></Button>}
               <Button type="button" size="icon" variant="ghost" className="h-7 w-7 shrink-0" disabled={locked} aria-label={`${t("siteFiles.browserMove" as never)} ${entry.name}`} title={t("siteFiles.browserMove" as never)} onClick={(event) => { moveOpenerRef.current = event.currentTarget; setMoveError(null); setMovePath(entry.path); setMoveEntry(entry); }}><Move className="size-3.5" /></Button>
               <Button type="button" size="icon" variant="ghost" className="h-7 w-7 shrink-0" disabled={locked} aria-label={`${t("siteFiles.browserRename" as never)} ${entry.name}`} title={t("siteFiles.browserRename" as never)} onClick={() => { setRenameError(null); setRenameName(entry.name); setRenameEntry(entry); }}><Pencil className="size-3.5" /></Button>
               <Button type="button" size="icon" variant="ghost" className="h-7 w-7 shrink-0 text-error hover:text-error" disabled={locked} aria-label={`${t("siteFiles.browserDelete" as never)} ${entry.name}`} title={t("siteFiles.browserDelete" as never)} onClick={() => { setDeleteError(null); setDeleteConfirmation(""); setDeleteEntry(entry); }}><Trash2 className="size-3.5" /></Button>
             </div>
           </div>)}
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
    <Dialog open={createKind !== null} onOpenChange={(open) => { if (!busy && !open) { setCreateKind(null); setCreateName(""); setCreateError(null); } }}>
      <DialogContent className="max-w-md" hideClose={busy}>
        <DialogHeader><DialogTitle>{t(createKind === "directory" ? "siteFiles.browserNewFolderTitle" as never : "siteFiles.browserNewFileTitle" as never)}</DialogTitle><DialogDescription>{t("siteFiles.browserCreateHint" as never)}</DialogDescription></DialogHeader>
        <form className="space-y-4" onSubmit={createEntry}>
          <div className="space-y-1.5"><Label htmlFor="site-file-create-name">{t(createKind === "directory" ? "siteFiles.browserFolderName" as never : "siteFiles.browserFileName" as never)}</Label><Input id="site-file-create-name" value={createName} disabled={busy} autoComplete="off" spellCheck={false} placeholder={t("siteFiles.browserNamePlaceholder" as never)} aria-invalid={!!createName && !relativeNameValid(createName)} onChange={(event) => setCreateName(event.target.value)} /><p className="text-xs leading-relaxed text-muted">{t("siteFiles.browserNameHint" as never)}</p></div>
          {createError && <p role="alert" className="break-words text-xs text-error">{createError}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={() => setCreateKind(null)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !relativeNameValid(createName)}>{busy && <Loader2 className="size-3.5 animate-spin" />}{t("siteFiles.browserCreateAction" as never)}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={renameEntry !== null} onOpenChange={(open) => { if (!busy && !open) { setRenameEntry(null); setRenameName(""); setRenameError(null); } }}>
      <DialogContent className="max-w-md" hideClose={busy}>
        <DialogHeader><DialogTitle>{t("siteFiles.browserRenameTitle" as never)}</DialogTitle><DialogDescription className="break-all font-mono">{renameEntry?.path}</DialogDescription></DialogHeader>
        <form className="space-y-4" onSubmit={renameEntryNow}>
          <div className="space-y-1.5"><Label htmlFor="site-file-rename-name">{t("siteFiles.browserNewName" as never)}</Label><Input id="site-file-rename-name" value={renameName} disabled={busy} autoComplete="off" spellCheck={false} aria-invalid={!!renameName && !relativeNameValid(renameName)} onChange={(event) => setRenameName(event.target.value)} /></div>
          {renameError && <p role="alert" className="break-words text-xs text-error">{renameError}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={() => setRenameEntry(null)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !relativeNameValid(renameName) || renameName.trim() === renameEntry?.name}>{busy && <Loader2 className="size-3.5 animate-spin" />}{t("siteFiles.browserRenameAction" as never)}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={moveEntry !== null} onOpenChange={(open) => { if (!busy && !open) { setMoveEntry(null); setMovePath(""); setMoveError(null); } }}>
      <DialogContent className="max-w-md" hideClose={busy} onCloseAutoFocus={(event) => { event.preventDefault(); moveOpenerRef.current?.focus(); }}>
        <DialogHeader><DialogTitle>{t("siteFiles.browserMoveTitle" as never)}</DialogTitle><DialogDescription className="break-all font-mono">{moveEntry?.path}</DialogDescription></DialogHeader>
        <form className="space-y-4" onSubmit={moveEntryNow}>
          <div className="space-y-1.5"><Label htmlFor="site-file-move-path">{t("siteFiles.browserTargetPath" as never)}</Label><Input id="site-file-move-path" value={movePath} disabled={busy} autoComplete="off" spellCheck={false} aria-invalid={!!movePath && !relativePathValid(movePath)} onChange={(event) => setMovePath(event.target.value)} /><p className="text-xs leading-relaxed text-muted">{t("siteFiles.browserMoveHint" as never)}</p></div>
          {moveError && <p role="alert" className="break-words text-xs text-error">{moveError}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={() => setMoveEntry(null)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !relativePathValid(movePath) || movePath.trim().replaceAll("\\", "/") === moveEntry?.path}>{busy && <Loader2 className="size-3.5 animate-spin" />}{t("siteFiles.browserMoveAction" as never)}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={deleteEntry != null} onOpenChange={(open) => { if (!busy && !open) { setDeleteEntry(null); setDeleteConfirmation(""); setDeleteError(null); } }} title={t("siteFiles.browserDeleteTitle" as never)} description={t("siteFiles.browserDeleteHint" as never)} confirmText={t("siteFiles.browserDeleteAction" as never)} loading={busy} confirmDisabled={!deleteEntry || deleteConfirmation !== deleteEntry.path} danger onConfirm={() => void deleteEntryNow()}>
      <div className="space-y-3"><div className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t(deleteEntry?.directory ? "siteFiles.browserDeleteDirectoryHint" as never : "siteFiles.browserDeleteFileHint" as never)}</div><p className="break-all rounded-lg bg-fill p-3 font-mono text-xs">{deleteEntry?.path}</p><div className="space-y-1.5"><Label htmlFor="site-file-delete-confirm">{t("siteFiles.browserDeleteConfirm" as never)}</Label><Input id="site-file-delete-confirm" value={deleteConfirmation} disabled={busy} autoComplete="off" spellCheck={false} placeholder={t("siteFiles.browserDeletePlaceholder" as never)} onChange={(event) => setDeleteConfirmation(event.target.value)} /></div>{deleteError && <p role="alert" className="break-words text-xs text-error">{deleteError}</p>}</div>
    </ConfirmDialog>
  </section>;
}
