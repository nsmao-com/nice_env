"use client";

import * as React from "react";
import Link from "next/link";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ArchiveRestore, DatabaseBackup, Loader2, RefreshCw } from "lucide-react";
import type { MongoBackup, MongoRestorePreview, ServiceStatus } from "@nsb/schema";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CopyButton } from "@/components/shared/misc";

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
  return <Card>
    <CardHeader><div className="flex flex-wrap items-center justify-between gap-2"><CardTitle>{t("mongoBackup.title")}</CardTitle><Button variant="ghost" size="sm" disabled={busy || backups.isFetching} onClick={() => void backups.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("mongo.refresh")}</Button></div><CardDescription>{t("mongoBackup.intro")}</CardDescription></CardHeader>
    <CardContent className="min-w-0 space-y-4">
      <p className="text-xs leading-5 text-muted">{t("mongoBackup.pauseWrites")} <Link href="/packages" className="underline underline-offset-4">{t("mongo.packages")}</Link></p>
      {!running && <p className="text-sm text-muted">{t("mongoBackup.stopped")}</p>}
      {overview.isError && running && <div className="space-y-2"><Failure error={overview.error} /><Button size="sm" variant="secondary" disabled={busy || overview.isFetching} onClick={() => void overview.refetch()}>{t("mongo.retry")}</Button></div>}
      <div className="flex flex-col items-stretch gap-3 sm:flex-row sm:items-end"><div className="min-w-0 flex-1 space-y-2"><Label htmlFor="mongo-backup-database">{t("mongoBackup.source")}</Label><Select value={databases.includes(database) ? database : ""} onValueChange={setDatabase} disabled={!ready || busy}><SelectTrigger id="mongo-backup-database"><SelectValue placeholder={t("mongoBackup.chooseDatabase")} /></SelectTrigger><SelectContent>{databases.map(name => <SelectItem key={name} value={name} className="break-all">{name}</SelectItem>)}</SelectContent></Select></div><Button disabled={!ready || busy || !databases.includes(database)} onClick={() => void create()}><DatabaseBackup className="h-4 w-4" />{t("mongoBackup.create")}</Button></div>
      {ready && !databases.length && <p className="text-xs text-muted">{t("mongoBackup.noDatabases")}</p>}
      {busy && <p role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 shrink-0 animate-spin" />{t("mongoBackup.working")}</p>}
      {!!error && <Failure error={error} />}{restored && <p role="status" className="break-all text-sm text-running">{restored}</p>}
      <div className="min-w-0 space-y-3 border-t border-dashed border-border pt-4">
        <h3 className="text-sm font-medium">{t("mongoBackup.history")}</h3>
        {backups.isPending && <p role="status" className="text-sm text-muted">{t("mongo.loading")}</p>}
        {backups.isError ? <Failure error={backups.error} /> : backups.data && <>
          <div className="flex min-w-0 items-center gap-2"><code className="min-w-0 break-all text-xs text-muted">{backups.data.directory}</code><CopyButton text={backups.data.directory} className="shrink-0" /></div>
          {backups.data.unreadable > 0 && <p role="alert" className="text-xs text-warn">{t("mongoBackup.unreadable").replace("{count}", String(backups.data.unreadable))}</p>}
          {!backups.data.items.length && <p className="py-3 text-sm text-muted">{t("mongoBackup.empty")}</p>}
          <div className="max-h-96 space-y-3 overflow-y-auto">{backups.data.items.map(backup => <div key={backup.id} className="flex min-w-0 flex-col gap-3 rounded-xl bg-fill p-3 sm:flex-row sm:items-center sm:justify-between"><div className="min-w-0 space-y-1"><div className="flex flex-wrap items-center gap-2"><span className="break-all text-sm font-medium">{backup.database}</span>{backup.kind === "before-restore" && <Badge>{t("mongoBackup.safety")}</Badge>}</div><p className="break-words text-xs text-muted">{new Date(backup.createdAt * 1000).toLocaleString()} · {size(backup.sizeBytes)} · MongoDB {backup.version}</p><details className="text-xs text-muted"><summary className="cursor-pointer">{t("mongoBackup.details")}</summary><div className="space-y-1 py-2"><p className="break-all">{backup.id}</p><p>Database Tools {backup.toolsVersion}</p><p className="break-all">SHA-256 {backup.sha256}</p></div></details></div><Button variant="secondary" size="sm" className="shrink-0" disabled={!ready || busy} onClick={() => openRestore(backup)}><ArchiveRestore className="h-3.5 w-3.5" />{t("mongoBackup.restore")}</Button></div>)}</div>
        </>}
      </div>
    </CardContent>
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
