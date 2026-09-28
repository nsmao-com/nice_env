"use client";

import * as React from "react";
import Link from "next/link";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ArchiveRestore, DatabaseBackup, Download, Ellipsis, FolderOpen, Loader2, RefreshCw, Trash2, Upload } from "lucide-react";
import type { MongoBackup, MongoBackupRemoval, MongoImportPreview, MongoRestorePreview, ServiceStatus } from "@nsb/schema";
import * as api from "@/lib/api";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CopyButton } from "@/components/shared/misc";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

const validName = (value: string) => !!value && new TextEncoder().encode(value).length < 64 && !/[\s\x00-\x1f\x7f-\x9f/\\."$*<>:|?]/.test(value) && !["admin", "local", "config"].includes(value.toLowerCase());
const size = (bytes: number) => bytes < 1048576 ? `${(bytes/1024).toFixed(1)} KB` : `${(bytes/1048576).toFixed(1)} MB`;

function Failure({ error }: { error: unknown }) {
  const t = useT(); const parsed = normalizeError(error);
  return <div role="alert" className="min-w-0 space-y-2 text-sm text-error"><p className="break-words">{parsed.message}</p>{parsed.hint && <p className="break-words text-xs leading-5">{parsed.hint}</p>}{parsed.detail && <details className="text-muted"><summary className="cursor-pointer text-xs">{t("mongoBackup.details")}</summary><pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-all text-xs">{parsed.detail}</pre></details>}</div>;
}

export function MongoBackupPanel({ service, signature }: { service?: ServiceStatus; signature: string }) {
  const t = useT(); const client = useQueryClient();
  const running = !!service?.version && (service.state === "running" || (service.state === "error" && service.pids.length > 0));
  const version = service?.version ?? "";
  const [busy, setBusy] = React.useState(false); const busyRef = React.useRef(false);
  const [error, setError] = React.useState<unknown>(null);
  const [database, setDatabase] = React.useState("");
  const [selected, setSelected] = React.useState<MongoBackup | null>(null);
  const [dialogSignature, setDialogSignature] = React.useState("");
  const [mode, setMode] = React.useState("new"); const [target, setTarget] = React.useState("");
  const [preview, setPreview] = React.useState<MongoRestorePreview | null>(null);
  const [confirmation, setConfirmation] = React.useState("");
  const confirmationRef = React.useRef<HTMLInputElement>(null);
  React.useEffect(() => { if (preview) confirmationRef.current?.focus(); }, [preview]);
  const [dialogError, setDialogError] = React.useState<unknown>(null);
  const [restored, setRestored] = React.useState<string>("");
  const [importPreview, setImportPreview] = React.useState<MongoImportPreview | null>(null);
  const [importDatabase, setImportDatabase] = React.useState("");
  const [removal, setRemoval] = React.useState<MongoBackupRemoval | null>(null);
  const [fileError, setFileError] = React.useState<unknown>(null);
  const alive = React.useRef(true);
  React.useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const signatureRef = React.useRef(signature); signatureRef.current = signature;
  const overview = useQuery({ queryKey: ["mongo-overview", signature], queryFn: () => api.mongoOverview(version), enabled: running && !busy, retry: false, refetchOnWindowFocus: false });
  const backups = useQuery({ queryKey: ["mongo-backups"], queryFn: api.mongoBackupList, enabled: !busy, retry: false, refetchOnWindowFocus: false });
  const databases = (overview.data?.databases ?? []).filter(validName);
  const ready = running && !!overview.data && !overview.isError && !overview.isFetching;
  const unchanged = signature === dialogSignature && running;
  const begin = () => { if (busyRef.current) return false; busyRef.current = true; setBusy(true); return true; };
  const end = () => { busyRef.current = false; setBusy(false); };
  const resetPreview = () => { setPreview(null); setConfirmation(""); setDialogError(null); };
  const refresh = async () => { await client.invalidateQueries({ queryKey: ["mongo-backups"] }); };
  const create = async () => {
    if (!ready || !databases.includes(database) || !begin()) return;
    setError(null); setRestored("");
    try { await api.mongoBackupCreate(version, database); toast.success(t("mongoBackup.created")); await refresh(); }
    catch (e) { setError(e); } finally { end(); }
  };
  const openRestore = (backup: MongoBackup) => {
    setSelected(backup); setDialogSignature(signature); setMode("new"); setTarget(""); resetPreview(); setRestored("");
  };
  const inspect = async () => {
    if (!selected || !unchanged || !validName(target) || (mode === "existing" && !databases.includes(target)) || !begin()) return;
    resetPreview(); const requestSignature = signature;
    try {
      const result = await api.mongoRestorePreview(version, selected.id, target);
      if (requestSignature !== signatureRef.current) return;
      if (mode === "new" && result.exists) throw { message: t("mongoBackup.nameExists") };
      if (mode === "existing" && !result.exists) throw { message: t("mongoBackup.targetMissing") };
      setPreview(result);
    } catch (e) { setDialogError(e); } finally { end(); }
  };
  const restore = async () => {
    if (!selected || !preview || !unchanged || preview.target !== target || confirmation !== target || !begin()) return;
    setDialogError(null);
    try {
      const result = await api.mongoBackupRestore(version, selected.id, target, preview.revision, confirmation);
      setRestored(result.safetyBackup ? t("mongoBackup.safetySaved").replace("{id}", result.safetyBackup.id) : t("mongoBackup.restored"));
      setSelected(null); toast.success(t("mongoBackup.restored"));
    } catch (e) { setDialogError(e); setPreview(null); setConfirmation(""); }
    finally {
      await refresh(); await Promise.all(["mongo-overview", "mongo-collections", "mongo-documents"].map(key => client.invalidateQueries({ queryKey: [key] })));
      end();
    }
  };
  const fileAction = async (work: () => Promise<void>, inDialog = false) => {
    if (!begin()) return;
    setError(null); setFileError(null); setRestored("");
    try { await work(); } catch (e) { if (alive.current) { if (inDialog) setFileError(e); else setError(e); } }
    finally { end(); }
  };
  const chooseImport = (previous?: string) => void fileAction(async () => {
    let source = previous ?? "preview/external/mongodb.archive.gz";
    if (isTauri && !previous) {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const chosen = await open({ title: t("mongoBackup.import"), multiple: false, directory: false, filters: [{ name: "MongoDB archive", extensions: ["gz", "archive"] }, { name: t("mongoBackup.allFiles"), extensions: ["*"] }] });
      if (typeof chosen !== "string") return; source = chosen;
    }
    if (!alive.current) return;
    const value = await api.mongoBackupInspectImport(source);
    if (!alive.current) return;
    const databases = value.info.databases.filter(db => validName(db.name));
    setImportDatabase(databases.length === 1 ? databases[0].name : ""); setImportPreview(value);
  }, !!previous);
  const importFile = () => {
    if (!importPreview || !importPreview.info.databases.some(db => db.name === importDatabase) || !validName(importDatabase)) return;
    const value = importPreview;
    void fileAction(async () => {
      await api.mongoBackupImport(value.source, importDatabase, value.revision);
      if (alive.current) { setImportPreview(null); setRestored(t("mongoBackup.importedDone")); }
      await refresh();
    }, true);
  };
  const exportFile = (backup: MongoBackup) => void fileAction(async () => {
    let destination = `preview/export/mongodb-${backup.id}.archive.gz`;
    if (isTauri) {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const chosen = await save({ title: t("mongoBackup.export"), defaultPath: `mongodb-${backup.id}.archive.gz`, filters: [{ name: "MongoDB gzip archive", extensions: ["gz"] }] });
      if (typeof chosen !== "string") return; destination = chosen;
    }
    if (!alive.current) return;
    const path = await api.mongoBackupExport(backup.id, destination);
    if (alive.current) setRestored(t("mongoBackup.exportedDone").replace("{path}", path));
  });
  const inspectDelete = (id: string) => void fileAction(async () => { const value = await api.mongoBackupRemovalPreview(id); if (alive.current) setRemoval(value); }, !!removal);
  const deleteFile = () => {
    if (!removal) return; const value = removal;
    void fileAction(async () => {
      await api.mongoBackupDelete(value.id, value.revision);
      if (alive.current) { setRemoval(null); setRestored(t("mongoBackup.deletedDone")); } await refresh();
    }, true);
  };
  const more = (backup: MongoBackup) => <DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm" disabled={busy} aria-label={`${t("mongoBackup.more")} ${backup.database} ${backup.id}`}><Ellipsis className="h-4 w-4" /></Button></DropdownMenuTrigger><DropdownMenuContent align="end"><DropdownMenuItem disabled={busy} onSelect={() => exportFile(backup)}><Download className="h-3.5 w-3.5" />{t("mongoBackup.export")}</DropdownMenuItem><DropdownMenuSeparator /><DropdownMenuItem disabled={busy} className="text-error focus:text-error" onSelect={() => inspectDelete(backup.id)}><Trash2 className="h-3.5 w-3.5" />{t("mongoBackup.delete")}</DropdownMenuItem></DropdownMenuContent></DropdownMenu>;
  return <Card>
    <CardHeader><div className="flex flex-wrap items-center justify-between gap-2"><CardTitle>{t("mongoBackup.title")}</CardTitle><Button variant="ghost" size="sm" disabled={busy || backups.isFetching} onClick={() => void backups.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("mongo.refresh")}</Button></div><CardDescription>{t("mongoBackup.intro")}</CardDescription></CardHeader>
    <CardContent className="min-w-0 space-y-4">
      {!isTauri && <p className="text-xs text-warn">{t("mongoBackup.demo")}</p>}
      <p className="text-xs leading-5 text-muted">{t("mongoBackup.pauseWrites")} <Link href="/packages" className="underline underline-offset-4">{t("mongo.packages")}</Link></p>
      {!running && <p className="text-sm text-muted">{t("mongoBackup.stopped")}</p>}
      {overview.isError && running && <div className="space-y-2"><Failure error={overview.error} /><Button size="sm" variant="secondary" disabled={busy || overview.isFetching} onClick={() => void overview.refetch()}>{t("mongo.retry")}</Button></div>}
      <div className="flex flex-col items-stretch gap-3 sm:flex-row sm:items-end"><div className="min-w-0 flex-1 space-y-2"><Label htmlFor="mongo-backup-database">{t("mongoBackup.source")}</Label><Select value={databases.includes(database) ? database : ""} onValueChange={setDatabase} disabled={!ready || busy}><SelectTrigger id="mongo-backup-database"><SelectValue placeholder={t("mongoBackup.chooseDatabase")} /></SelectTrigger><SelectContent>{databases.map(name => <SelectItem key={name} value={name} className="break-all">{name}</SelectItem>)}</SelectContent></Select></div><Button disabled={!ready || busy || !databases.includes(database)} onClick={() => void create()}><DatabaseBackup className="h-4 w-4" />{t("mongoBackup.create")}</Button></div>
      {ready && !databases.length && <p className="text-xs text-muted">{t("mongoBackup.noDatabases")}</p>}
      {busy && <p role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 shrink-0 animate-spin" />{t("mongoBackup.working")}</p>}
      {!!error && <Failure error={error} />}{restored && <p role="status" className="break-all text-sm text-running">{restored}</p>}
      <div className="min-w-0 space-y-3 border-t border-dashed border-border pt-4">
        <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-medium">{t("mongoBackup.history")}</h3><div className="flex flex-wrap gap-1"><Button variant="secondary" size="sm" disabled={busy} onClick={() => chooseImport()}><Upload className="h-3.5 w-3.5" />{t("mongoBackup.import")}</Button><Button variant="ghost" size="sm" disabled={busy || !isTauri || !backups.data} onClick={() => void fileAction(async () => { await api.openInFolder(backups.data!.directory); })}><FolderOpen className="h-3.5 w-3.5" />{t("mongoBackup.folder")}</Button></div></div>
        {backups.isPending && <p role="status" className="text-sm text-muted">{t("mongo.loading")}</p>}
        {backups.isError ? <Failure error={backups.error} /> : backups.data && <>
          <div className="flex min-w-0 items-center gap-2"><code className="min-w-0 break-all text-xs text-muted">{backups.data.directory}</code><CopyButton text={backups.data.directory} className="shrink-0" /></div>
          {backups.data.unreadable > 0 && <p role="alert" className="text-xs text-warn">{t("mongoBackup.unreadable").replace("{count}", String(backups.data.unreadable))}</p>}
          {(backups.data.issues ?? []).map(issue => <div key={issue.id} className="flex min-w-0 flex-col gap-2 rounded-lg bg-fill p-3 sm:flex-row sm:items-center sm:justify-between"><div className="min-w-0"><p className="break-all text-xs text-muted">{issue.id}</p><p className="break-words text-xs text-warn">{issue.problem}</p></div><Button variant="ghost" size="sm" disabled={busy} className="shrink-0 text-error" onClick={() => inspectDelete(issue.id)}><Trash2 className="h-3.5 w-3.5" />{t("mongoBackup.delete")}</Button></div>)}
          {!backups.data.items.length && <p className="py-3 text-sm text-muted">{t("mongoBackup.empty")}</p>}
          <div className="max-h-96 space-y-3 overflow-y-auto">{backups.data.items.map(backup => <div key={backup.id} className="flex min-w-0 flex-col gap-3 rounded-xl bg-fill p-3 sm:flex-row sm:items-center sm:justify-between"><div className="min-w-0 space-y-1"><div className="flex flex-wrap items-center gap-2"><span className="break-all text-sm font-medium">{backup.database}</span>{backup.kind === "before-restore" && <Badge>{t("mongoBackup.safety")}</Badge>}{backup.kind === "imported" && <Badge>{t("mongoBackup.imported")}</Badge>}</div><p className="break-words text-xs text-muted">{new Date(backup.createdAt * 1000).toLocaleString()} · {size(backup.sizeBytes)} · MongoDB {backup.version}</p><details className="text-xs text-muted"><summary className="cursor-pointer">{t("mongoBackup.details")}</summary><div className="space-y-1 py-2"><p className="break-all">{backup.id}</p><p>Database Tools {backup.toolsVersion}</p><p className="break-all">SHA-256 {backup.sha256}</p></div></details></div><div className="flex shrink-0 items-center justify-end gap-1"><Button variant="secondary" size="sm" className="shrink-0" disabled={!ready || busy} onClick={() => openRestore(backup)}><ArchiveRestore className="h-3.5 w-3.5" />{t("mongoBackup.restore")}</Button>{more(backup)}</div></div>)}</div>
        </>}
      </div>
    </CardContent>
    <Dialog open={!!importPreview} onOpenChange={open => { if (!open && !busyRef.current) setImportPreview(null); }}><DialogContent hideClose={busy} className="flex max-h-[85dvh] max-w-xl flex-col overflow-hidden p-5 sm:p-6" onInteractOutside={event => event.preventDefault()}>
      <DialogHeader className="shrink-0 pr-6"><DialogTitle>{t("mongoBackup.import")}</DialogTitle><DialogDescription>{t("mongoBackup.importIntro")}</DialogDescription></DialogHeader>
      <div className="min-h-0 space-y-4 overflow-y-auto pr-1">
        {importPreview && <><p className="break-all text-xs text-muted">{importPreview.source}</p><div className="flex flex-wrap gap-x-4 gap-y-2 text-xs"><span>MongoDB {importPreview.info.version}</span><span>Database Tools {importPreview.info.toolsVersion}</span><span>{size(importPreview.sizeBytes)}</span></div><div className="space-y-2"><Label htmlFor="mongo-import-database">{t("mongoBackup.importDatabase")}</Label><Select value={importDatabase} disabled={busy} onValueChange={setImportDatabase}><SelectTrigger id="mongo-import-database" className="h-auto min-h-9 whitespace-normal text-left [&>span]:line-clamp-none [&>span]:min-w-0 [&>span]:break-all [&>svg]:shrink-0"><SelectValue placeholder={t("mongoBackup.chooseDatabase")} /></SelectTrigger><SelectContent>{importPreview.info.databases.filter(db => validName(db.name)).map(db => <SelectItem value={db.name} key={db.name} className="break-all">{db.name} · {t("mongoBackup.collectionCount").replace("{count}", String(db.collections))}</SelectItem>)}</SelectContent></Select></div><p className="text-xs leading-5 text-muted">{t("mongoBackup.importScope")}</p><details className="text-xs text-muted"><summary className="cursor-pointer">{t("mongoBackup.details")}</summary><p className="break-all pt-2">SHA-256 {importPreview.sha256}</p></details></>}
        {!!fileError && <Failure error={fileError} />}{busy && <p role="status" className="text-sm text-muted">{t("mongoBackup.working")}</p>}
      </div><DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-border pt-4"><Button variant="ghost" disabled={busy} onClick={() => setImportPreview(null)}>{t("common.cancel")}</Button>{!!fileError && <Button variant="secondary" disabled={busy} onClick={() => chooseImport(importPreview!.source)}>{t("mongoBackup.reinspect")}</Button>}<Button disabled={busy || !validName(importDatabase)} onClick={importFile}>{t("mongoBackup.confirmImport")}</Button></DialogFooter>
    </DialogContent></Dialog>
    <Dialog open={!!removal} onOpenChange={open => { if (!open && !busyRef.current) setRemoval(null); }}><DialogContent hideClose={busy} className="flex max-h-[85dvh] max-w-lg flex-col overflow-hidden p-5 sm:p-6" onInteractOutside={event => event.preventDefault()}>
      <DialogHeader className="shrink-0 pr-6"><DialogTitle>{t("mongoBackup.delete")}</DialogTitle><DialogDescription>{t("mongoBackup.deleteIntro")}</DialogDescription></DialogHeader><div className="min-h-0 space-y-3 overflow-y-auto pr-1"><p className="break-all text-sm">{removal?.database ?? t("mongoBackup.unknownDatabase")}</p><p className="break-all text-xs text-muted">{removal?.id}</p>{removal?.sizeBytes != null && <p className="text-xs text-muted">{size(removal.sizeBytes)}</p>}{removal?.kind === "before-restore" && <p className="text-sm text-warn">{t("mongoBackup.deleteSafety")}</p>}{!!fileError && <Failure error={fileError} />}{busy && <p role="status" className="text-sm text-muted">{t("mongoBackup.working")}</p>}</div><DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-border pt-4"><Button variant="ghost" disabled={busy} onClick={() => setRemoval(null)}>{t("common.cancel")}</Button>{!!fileError && <Button variant="secondary" disabled={busy} onClick={() => inspectDelete(removal!.id)}>{t("mongoBackup.reinspect")}</Button>}<Button variant="destructive" disabled={busy} onClick={deleteFile}>{t("mongoBackup.confirmDelete")}</Button></DialogFooter>
    </DialogContent></Dialog>
    <Dialog open={!!selected} onOpenChange={open => { if (!open && !busyRef.current) setSelected(null); }}><DialogContent hideClose={busy} className="flex max-h-[85dvh] max-w-xl flex-col overflow-hidden p-5 sm:p-6" onEscapeKeyDown={event => { if (busyRef.current) event.preventDefault(); }} onInteractOutside={event => event.preventDefault()}>
      <DialogHeader className="shrink-0 pr-7"><DialogTitle>{t("mongoBackup.restore")}</DialogTitle><DialogDescription className="break-words">{t("mongoBackup.restoreIntro").replace("{database}", selected?.database ?? "")}</DialogDescription></DialogHeader>
      <div className="min-h-0 space-y-4 overflow-y-auto pr-1">
        {!unchanged && <p role="alert" className="text-sm text-warn">{t("mongoBackup.changed")}</p>}
        <div className="space-y-2"><Label htmlFor="mongo-restore-mode">{t("mongoBackup.destination")}</Label><Select value={mode} disabled={busy || !unchanged} onValueChange={next => { setMode(next); setTarget(""); resetPreview(); }}><SelectTrigger id="mongo-restore-mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="new">{t("mongoBackup.newDatabase")}</SelectItem><SelectItem value="existing">{t("mongoBackup.existingDatabase")}</SelectItem></SelectContent></Select></div>
        <div className="space-y-2"><Label htmlFor="mongo-restore-target">{t("mongoBackup.target")}</Label>{mode === "new" ? <Input id="mongo-restore-target" value={target} disabled={busy || !unchanged} placeholder="project_restored" onChange={event => { setTarget(event.target.value); resetPreview(); }} /> : <Select value={target} disabled={busy || !unchanged} onValueChange={value => { setTarget(value); resetPreview(); }}><SelectTrigger id="mongo-restore-target"><SelectValue placeholder={t("mongoBackup.chooseDatabase")} /></SelectTrigger><SelectContent>{databases.map(name => <SelectItem value={name} key={name} className="break-all">{name}</SelectItem>)}</SelectContent></Select>}{target && !validName(target) && <p role="alert" className="text-xs text-error">{t("mongoBackup.badName")}</p>}</div>
        <p className="text-xs leading-5 text-muted">{t("mongoBackup.pauseWrites")}</p>
        {preview && <div className="space-y-3 border-t border-dashed border-border pt-4"><p className="break-words text-sm leading-6">{t(preview.exists ? "mongoBackup.replaceWarning" : "mongoBackup.newWarning").replace("{target}", preview.target)}</p><p className="text-xs text-muted">{t("mongoBackup.verified")} · {size(preview.backup.sizeBytes)} · MongoDB {preview.backup.version}</p><div className="space-y-2"><Label htmlFor="mongo-restore-confirm" className="break-all">{t("mongoBackup.confirmName").replace("{target}", target)}</Label><Input ref={confirmationRef} id="mongo-restore-confirm" value={confirmation} autoComplete="off" disabled={busy || !unchanged} onChange={event => setConfirmation(event.target.value)} /></div></div>}
        {!!dialogError && <Failure error={dialogError} />}
        {busy && <p role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 shrink-0 animate-spin" />{t("mongoBackup.working")}</p>}
      </div>
      <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-border pt-4"><Button variant="ghost" disabled={busy} onClick={() => setSelected(null)}>{t("common.cancel")}</Button>{preview ? <Button variant={preview.exists ? "destructive" : "default"} disabled={busy || !unchanged || confirmation !== target} onClick={() => void restore()}>{t("mongoBackup.confirmRestore")}</Button> : <Button disabled={busy || !unchanged || !validName(target) || (mode === "existing" && !databases.includes(target))} onClick={() => void inspect()}>{t("mongoBackup.check")}</Button>}</DialogFooter>
    </DialogContent></Dialog>
  </Card>;
}
