"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { Archive, FolderOpen, Loader2, RotateCcw } from "lucide-react";
import type { RedisBackup, RedisRestorePreview } from "@nsb/schema";
import * as api from "@/lib/api";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { ConfirmDialog } from "./misc";

export function RedisBackupsButton({ version, running }: { version?: string | null; running?: boolean }) {
  const t = useT();
  const [target, setTarget] = React.useState<string | null>(null);
  return <><Button variant="secondary" size="sm" disabled={!version} onClick={() => setTarget(version!)}><Archive className="size-3.5" />{t("redisBackup.title")}</Button>
    {target && <RedisBackupsDialog key={target} version={target} running={!!running} onClose={() => setTarget(null)} />}</>;
}

function RedisBackupsDialog({ version, running, onClose }: { version: string; running: boolean; onClose: () => void }) {
  const t = useT();
  const query = useQuery({ queryKey: ["redis-backups"], queryFn: api.redisBackupList, retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const [busy, setBusy] = React.useState<"create" | "preview" | "restore" | "folder" | null>(null);
  const busyRef = React.useRef(false);
  const alive = React.useRef(true);
  React.useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [message, setMessage] = React.useState("");
  const [preview, setPreview] = React.useState<RedisRestorePreview | null>(null);
  const [confirmation, setConfirmation] = React.useState("");
  const [allVersions, setAllVersions] = React.useState(false);
  const [page, setPage] = React.useState(0);
  const errorRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => { if (error || query.isError) errorRef.current?.focus(); }, [error, query.isError]);
  const entries = (query.data?.items ?? []).filter((entry) => allVersions || entry.version === version);
  const pages = Math.max(1, Math.ceil(entries.length / 5));
  const index = Math.min(page, pages - 1);
  const failure = error ?? (query.isError ? normalizeError(query.error) : null);
  const size = fmtBytes;
  const perform = async (action: NonNullable<typeof busy>, work: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(action); setError(null); setMessage("");
    try { await work(); } catch (e) { if (alive.current) setError(normalizeError(e)); }
    finally { busyRef.current = false; if (alive.current) setBusy(null); }
  };
  const create = () => {
    if (!running) return;
    void perform("create", async () => {
      const entry = await api.redisBackupCreate(version);
      if (!alive.current) return;
      setAllVersions(false); setPage(0); setMessage(t("redisBackup.created").replace("{id}", entry.id)); await query.refetch();
    });
  };
  const inspect = (entry: RedisBackup) => {
    if (running || entry.version !== version) return;
    void perform("preview", async () => {
      setPreview(null); setConfirmation("");
      const value = await api.redisRestorePreview(version, entry.id);
      if (alive.current) setPreview(value);
    });
  };
  const restore = () => {
    if (!preview || running || confirmation !== `Redis ${version}`) return;
    const selected = preview;
    void perform("restore", async () => {
      const result = await api.redisBackupRestore(version, selected.backup.id, selected.revision, confirmation);
      if (!alive.current) return;
      setPreview(null); setConfirmation("");
      setMessage(t("redisBackup.restored").replace("{version}", version) + " " + (result.safetyBackup ? t("redisBackup.safetyCreated").replace("{id}", result.safetyBackup.id) : t("redisBackup.noPrevious")));
      await query.refetch();
    });
  };
  const errorBox = failure && <div ref={errorRef} role="alert" tabIndex={-1} className="space-y-1 rounded-lg bg-error-soft p-3 text-xs text-error outline-none [overflow-wrap:anywhere]"><p>{failure.message}</p>{failure.hint && <p>{failure.hint}</p>}</div>;
  return <>
    <Dialog open onOpenChange={(open) => { if (!open && !busyRef.current && !preview) onClose(); }}>
      <DialogContent hideClose={!!busy} className="flex max-h-[90dvh] max-w-2xl flex-col overflow-hidden p-4 sm:p-6">
        <DialogHeader className="shrink-0 pr-6"><DialogTitle className="leading-snug">Redis {version} · {t("redisBackup.title")}</DialogTitle><DialogDescription>{t("redisBackup.intro")}</DialogDescription></DialogHeader>
        <div className="min-h-0 space-y-4 overflow-y-auto px-0.5 text-xs leading-relaxed">
          {!isTauri && <p className="text-warn">{t("redisBackup.demo")}</p>}
          {query.isPending && <p role="status">{t("common.loading")}</p>}
          {!preview && errorBox}
          {message && <p role="status" className="rounded-lg bg-fill p-3 text-secondary [overflow-wrap:anywhere]">{message}</p>}
          {busy && <p role="status" className="flex items-center gap-2"><Loader2 className="size-3.5 shrink-0 animate-spin motion-reduce:animate-none" />{t(`redisBackup.busy.${busy}`)}</p>}
          <div className="space-y-2 rounded-lg bg-fill p-3 text-muted"><p>{t(running ? "redisBackup.runningHint" : "redisBackup.stoppedHint")}</p><p>{t("redisBackup.sharedHint")}</p></div>
          {!!query.data?.unreadable && <p role="alert" className="text-warn">{t("redisBackup.unreadable").replace("{n}", String(query.data.unreadable))}</p>}
          <label className="flex items-center gap-2"><input type="checkbox" className="size-4 accent-primary" checked={allVersions} disabled={!!busy} onChange={(event) => { setAllVersions(event.target.checked); setPage(0); }} />{t("redisBackup.allVersions")}</label>
          {!query.isPending && !query.isError && !entries.length && <p className="rounded-lg bg-fill p-5 text-center text-muted">{t("redisBackup.empty")}</p>}
          <ul className="space-y-3">{entries.slice(index * 5, index * 5 + 5).map((entry) => <li key={entry.id} className="space-y-2 rounded-xl bg-fill p-3">
            <div className="flex flex-wrap items-center justify-between gap-2"><p className="font-medium">{new Date(entry.createdAt).toLocaleString()}</p><span className="text-muted">Redis {entry.version} · {size(entry.sizeBytes)}</span></div>
            <p className="text-muted">{t(entry.kind === "before-restore" ? "redisBackup.safety" : "redisBackup.snapshot")}</p>
            <div className="flex flex-wrap items-center justify-between gap-2 border-t border-dashed border-separator pt-2"><code className="min-w-0 text-[11px] text-faint [overflow-wrap:anywhere]">{entry.id}</code><Button variant="ghost" size="sm" disabled={!!busy || running || entry.version !== version} onClick={() => inspect(entry)}><RotateCcw className="size-3.5" />{t("redisBackup.inspect")}</Button></div>
            {entry.version !== version && <p className="text-faint">{t("redisBackup.versionHint")}</p>}
          </li>)}</ul>
          {pages > 1 && <div className="flex flex-wrap items-center justify-between gap-2"><Button variant="ghost" size="sm" disabled={!!busy || index === 0} onClick={() => setPage(index - 1)}>{t("redisBackup.previous")}</Button><span className="text-muted">{index + 1} / {pages}</span><Button variant="ghost" size="sm" disabled={!!busy || index + 1 >= pages} onClick={() => setPage(index + 1)}>{t("redisBackup.next")}</Button></div>}
          <p className="border-t border-dashed border-separator pt-3 text-muted">{t("redisBackup.offDevice")}</p>
        </div>
        <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-3">
          <Button variant="ghost" size="sm" disabled={!!busy || !isTauri || !query.data} onClick={() => void perform("folder", async () => { await api.openInFolder(query.data!.directory); })}><FolderOpen className="size-3.5" />{t("redisBackup.folder")}</Button>
          <Button variant="ghost" size="sm" disabled={!!busy || query.isFetching} onClick={() => { setError(null); void query.refetch(); }}>{t("redisBackup.refresh")}</Button>
          <Button variant="ghost" size="sm" disabled={!!busy} onClick={onClose}>{t("common.close")}</Button>
          <Button size="sm" disabled={!!busy || !running} onClick={create}>{t("redisBackup.create")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={!!preview} onOpenChange={(open) => { if (!open && !busyRef.current) { setPreview(null); setConfirmation(""); setError(null); } }} title={t("redisBackup.restoreTitle")} description={t("redisBackup.restoreHint")} danger loading={busy === "restore"} confirmDisabled={running || confirmation !== `Redis ${version}`} confirmText={t("redisBackup.restore")} onConfirm={restore}>
      {preview && <div className="space-y-3 text-xs leading-relaxed">
        <p className="font-medium">Redis {version} · {new Date(preview.backup.createdAt).toLocaleString()} · {size(preview.backup.sizeBytes)}</p>
        <p className="text-muted">{t(preview.existingSize === null ? "redisBackup.noPrevious" : "redisBackup.safetyHint")}</p>
        <details className="space-y-2"><summary className="cursor-pointer text-muted">{t("redisBackup.details")}</summary><p className="font-mono text-faint [overflow-wrap:anywhere]">{preview.target}</p><p className="font-mono text-faint [overflow-wrap:anywhere]">SHA-256: {preview.backup.sha256}</p></details>
        <div className="space-y-1.5"><Label htmlFor="redis-restore-confirm">{t("redisBackup.confirm").replace("{target}", `Redis ${version}`)}</Label><Input id="redis-restore-confirm" value={confirmation} disabled={!!busy} autoComplete="off" spellCheck={false} onChange={(event) => setConfirmation(event.target.value)} /></div>
        {running && <p role="alert" className="text-error">{t("redisBackup.runningHint")}</p>}
        {errorBox}
        {error && <Button type="button" variant="ghost" size="sm" disabled={!!busy || running} onClick={() => inspect(preview.backup)}>{t("redisBackup.recheck")}</Button>}
      </div>}
    </ConfirmDialog>
  </>;
}
