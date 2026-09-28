"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, FolderOpen, RefreshCw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { isTauri, listen, normalizeError } from "@/lib/backend";
import { toastError } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog, CopyButton } from "@/components/shared/misc";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";

import { SiteFileBackupPlan } from "./site-file-backup-plan";

export function SiteFileBackups({ siteId, revision, active, disabled, dirty, onBusyChange }: {
  siteId: string; revision: number; active: boolean; disabled: boolean; dirty: boolean;
  onBusyChange: (busy: boolean) => void;
}) {
  const t = useT();
  const client = useQueryClient();
  const [project, setProject] = React.useState(true);
  const [exclude, setExclude] = React.useState(true);
  const [confirmed, setConfirmed] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const planBusyRef = React.useRef(false);
  const [planBusy, setPlanBusy] = React.useState(false);
  const onPlanBusyChange = React.useCallback((value: boolean) => { planBusyRef.current = value; setPlanBusy(value); onBusyChange(value || busyRef.current); }, [onBusyChange]);
  const [progress, setProgress] = React.useState<api.SiteFileProgress | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const errorRef = React.useRef<HTMLParagraphElement | null>(null);
  React.useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);
  const [search, setSearch] = React.useState("");
  const [page, setPage] = React.useState(1);
  const [restore, setRestore] = React.useState<api.SiteFileBackup | null>(null);
  const [deleting, setDeleting] = React.useState<api.SiteFileBackup | null>(null);
  const [parent, setParent] = React.useState<string | null>(null);
  const [trusted, setTrusted] = React.useState(false);
  const [restored, setRestored] = React.useState("");
  const [importOpen, setImportOpen] = React.useState(false);
  const [importPath, setImportPath] = React.useState("");
  const [importPreview, setImportPreview] = React.useState<api.SiteFileImportPreview | null>(null);
  const [importConfirmed, setImportConfirmed] = React.useState(false);
  const importOpener = React.useRef<HTMLButtonElement | null>(null);
  const restoreOpener = React.useRef<HTMLButtonElement | null>(null);
  const deleteOpener = React.useRef<HTMLButtonElement | null>(null);
  const queryKey = ["site-files", siteId];
  const scope = useQuery({ queryKey: ["site-files-scope", siteId, revision, project, exclude],
    queryFn: () => api.siteFilesScope(siteId, project, exclude), enabled: active && !dirty && !busy, retry: false, staleTime: 0 });
  const archives = useQuery({ queryKey, queryFn: () => api.siteFilesList(siteId), enabled: active && !busy, retry: false });
  React.useEffect(() => { setConfirmed(false); }, [project, exclude, revision, dirty, scope.data?.revision]);
  React.useEffect(() => { setImportConfirmed(false); setImportPreview(null); }, [revision, dirty]);
  const locked = busy || planBusy || disabled || dirty;
  const filtered = (archives.data ?? []).filter((item) => `${item.name} ${item.root}`.toLowerCase().includes(search.trim().toLowerCase()));
  const pages = Math.max(1, Math.ceil(filtered.length / 5));
  const currentPage = Math.min(page, pages);
  const setWorking = (value: boolean) => { busyRef.current = value; setBusy(value); onBusyChange(value || planBusyRef.current); };
  const run = async (action: (operationId: string) => Promise<void>) => {
    if (busyRef.current || planBusyRef.current || disabled || dirty) return;
    setWorking(true); setError(null); setProgress(null);
    const operationId = crypto.randomUUID();
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await listen<api.SiteFileProgress>("site-files://progress", (event) => {
        if (event.siteId === siteId && event.operationId === operationId) setProgress(event);
      });
      await action(operationId);
    } catch (failure) {
      const detail = normalizeError(failure);
      setError([detail.message, detail.hint].filter(Boolean).join(" · "));
      if (importOpen) { setImportPreview(null); setImportConfirmed(false); }
      if (detail.code === "SITE_BACKUP_CHANGED") { setConfirmed(false); void scope.refetch(); }
    } finally {
      unlisten?.(); setWorking(false); setProgress(null);
    }
  };
  const progressView = busy && <div role="status" className="space-y-1 rounded-lg bg-fill p-3 text-xs">
    <p className="flex items-center gap-2"><RefreshCw className="size-3.5 shrink-0 animate-spin" />{t(progress?.phase === "inspect" ? "siteImport.inspect" : progress?.phase === "importRead" ? "siteImport.reading" : progress?.phase === "import" ? "siteImport.importing" : progress?.phase === "scan" ? "siteFiles.scan" : progress?.phase === "backup" ? "siteFiles.backup" : progress?.phase === "restore" ? "siteFiles.restoring" : progress?.phase === "complete" ? "siteFiles.complete" : "siteFiles.working")}</p>
    {progress && <p className="tabular-nums text-muted">{t("siteFiles.files").replace("{count}", String(progress.files))} · {fmtBytes(progress.bytes)}</p>}
  </div>;
  const errorView = error && <p ref={errorRef} tabIndex={-1} role="alert" className="rounded-lg bg-error-soft p-3 text-xs leading-relaxed text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]">{error}</p>;
  return <div className="min-w-0 space-y-5">
    <div className="space-y-2">
      <h3 className="text-sm font-semibold">{t("siteFiles.title")}</h3>
      <p className="text-xs leading-relaxed text-muted">{t("siteFiles.hint")}</p>
      {!isTauri && <p className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t("siteFiles.preview")}</p>}
      {dirty && <p role="status" className="text-xs leading-relaxed text-warn">{t("siteFiles.dirty")}</p>}
    </div>
    <SiteFileBackupPlan siteId={siteId} revision={revision} active={active} disabled={busy || disabled || dirty || !!restore || !!deleting || importOpen} onBusyChange={onPlanBusyChange} />
    <div className="space-y-4 rounded-xl border border-border p-4">
      <div className="space-y-2">
        <Label htmlFor="site-files-scope">{t("siteFiles.source")}</Label>
        <Select value={project ? "project" : "web"} disabled={locked} onValueChange={(value) => { setConfirmed(false); setProject(value === "project"); }}>
          <SelectTrigger id="site-files-scope"><SelectValue /></SelectTrigger>
          <SelectContent><SelectItem value="project">{t("siteFiles.project")}</SelectItem><SelectItem value="web">{t("siteFiles.web")}</SelectItem></SelectContent>
        </Select>
        {scope.isFetching ? <p role="status" className="text-xs text-muted">{t("common.loading")}</p>
          : scope.error ? <div role="alert" className="space-y-2 text-xs text-error [overflow-wrap:anywhere]">
            <p>{normalizeError(scope.error).message}</p><Button size="sm" variant="secondary" disabled={locked} onClick={() => void scope.refetch()}>{t("siteFiles.retry")}</Button>
          </div> : scope.data && <p className="rounded-lg bg-fill p-2.5 font-mono text-xs [overflow-wrap:anywhere]">{scope.data.root}</p>}
      </div>
      <div className="space-y-2 border-t border-dashed border-separator pt-4">
        <label className="flex items-center justify-between gap-3 text-xs"><span>{t("siteFiles.exclude")}</span><Switch checked={exclude} disabled={locked} onCheckedChange={(value) => { setConfirmed(false); setExclude(value); }} /></label>
        {scope.data && <p className="text-xs leading-relaxed text-muted [overflow-wrap:anywhere]">{scope.data.excluded.length ? scope.data.excluded.join(", ") : t("siteFiles.noExclusions")}</p>}
        <p className="text-xs leading-relaxed text-muted">{t("siteFiles.includes")}</p>
        <p className="text-xs text-faint">{t("siteFiles.limits")}</p>
      </div>
      <label className="flex items-start gap-2.5 text-xs leading-relaxed">
        <input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={confirmed} disabled={locked || scope.isFetching || !!scope.error || !scope.data} onChange={(event) => setConfirmed(event.target.checked)} />
        <span>{t("siteFiles.confirm")}</span>
      </label>
      <Button className="w-full sm:w-auto" disabled={locked || !confirmed || !scope.data || !!scope.error || scope.isFetching} onClick={() => void run(async (operationId) => {
        const next = await api.siteFilesCreate(siteId, project, exclude, scope.data!.revision, confirmed, operationId);
        await client.cancelQueries({ queryKey });
        client.setQueryData<api.SiteFileBackup[]>(queryKey, (previous) => [next, ...(previous ?? []).filter((item) => item.name !== next.name)]);
        setConfirmed(false); setSearch(""); setPage(1); toast.success(t("siteFiles.created"));
      })}><Archive className="size-4" />{t("siteFiles.create")}</Button>
    </div>
    {!restore && !deleting && !importOpen && <>{progressView}{errorView}</>}
    {restored && <div role="status" className="space-y-2 rounded-xl bg-running-soft p-3">
      <p className="text-xs font-medium text-running">{t("siteFiles.restored")}</p>
      <p className="font-mono text-xs [overflow-wrap:anywhere]">{restored}</p>
      <div className="flex flex-wrap gap-2"><Button size="sm" variant="secondary" disabled={!isTauri} onClick={() => api.openInFolder(restored).catch(toastError)}><FolderOpen className="size-3.5" />{t("siteFiles.folder")}</Button><CopyButton text={restored} /></div>
    </div>}
    <section className="space-y-3 border-t border-dashed border-separator pt-4">
      <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-semibold">{t("siteFiles.history")} · {archives.data?.length ?? 0}</h3>
        <div className="flex flex-wrap gap-2"><Button size="sm" variant="secondary" disabled={locked} onClick={(event) => { importOpener.current = event.currentTarget; setError(null); setImportPath(""); setImportPreview(null); setImportConfirmed(false); setImportOpen(true); }}>{t("siteImport.open")}</Button>
        <Button size="sm" variant="ghost" disabled={locked || archives.isFetching} onClick={() => void archives.refetch()}><RefreshCw className="size-3.5" />{t("siteFiles.refresh")}</Button></div></div>
      <Input aria-label={t("siteFiles.search")} placeholder={t("siteFiles.search")} value={search} onChange={(event) => { setSearch(event.target.value); setPage(1); }} />
      {archives.isPending ? <p role="status" className="text-xs text-muted">{t("common.loading")}</p> : archives.error ? <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{normalizeError(archives.error).message}</p>
        : !filtered.length ? <div className="rounded-xl bg-fill p-4 text-xs leading-relaxed text-muted"><p>{t(search ? "siteFiles.noResults" : "siteFiles.empty")}</p>{search && <Button className="mt-2" size="sm" variant="ghost" onClick={() => { setSearch(""); setPage(1); }}>{t("siteFiles.reset")}</Button>}</div>
        : <ul className="space-y-3">{filtered.slice((currentPage - 1) * 5, currentPage * 5).map((item) => <li key={item.name} className="space-y-3 rounded-xl border border-border p-3">
          <div className="space-y-1.5"><p className="font-mono text-xs [overflow-wrap:anywhere]">{item.name}</p>
            {item.automatic && <p className="text-xs font-medium text-muted">{t("siteSchedule.automatic")}</p>}
            {item.createdAt > 0 && <p className="text-xs text-muted">{new Date(item.createdAt).toLocaleString()}</p>}
            <p className="text-xs text-muted [overflow-wrap:anywhere]">{t("siteFiles.zip")} {fmtBytes(item.sizeBytes)} · {t("siteFiles.files").replace("{count}", String(item.files))} · {t("siteFiles.original")} {fmtBytes(item.originalBytes)}</p>
            <p className="font-mono text-xs text-muted [overflow-wrap:anywhere]">{item.root}</p>
            {!item.restorable && <p className="text-xs text-error [overflow-wrap:anywhere]">{item.error || t("siteFiles.invalid")}</p>}
          </div>
          <div className="flex flex-wrap gap-2 border-t border-dashed border-separator pt-3">
            <Button size="sm" variant="secondary" disabled={locked || !item.restorable} onClick={(event) => { restoreOpener.current = event.currentTarget; setError(null); setParent(null); setTrusted(false); setRestore(item); }}>{t("siteFiles.restore")}</Button>
            <Button size="sm" variant="ghost" disabled={!isTauri || busy} onClick={() => api.openInFolder(item.path).catch(toastError)}><FolderOpen className="size-3.5" />{t("siteFiles.folder")}</Button>
            <Button size="sm" variant="ghost" className="text-error" disabled={locked} onClick={(event) => { deleteOpener.current = event.currentTarget; setError(null); setDeleting(item); }}><Trash2 className="size-3.5" />{t("common.delete")}</Button>
          </div>
        </li>)}</ul>}
      {pages > 1 && <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted">
        <span>{t("siteFiles.page").replace("{page}", String(currentPage)).replace("{total}", String(pages))}</span>
        <div className="flex gap-2"><Button size="sm" variant="secondary" disabled={currentPage === 1} onClick={() => setPage(currentPage - 1)}>{t("siteFiles.previous")}</Button><Button size="sm" variant="secondary" disabled={currentPage === pages} onClick={() => setPage(currentPage + 1)}>{t("siteFiles.next")}</Button></div>
      </div>}
    </section>
    <Dialog open={importOpen} onOpenChange={(open) => { if (!busyRef.current) { setImportOpen(open); setError(null); } }}>
      <DialogContent hideClose={busy} className="flex max-h-[85dvh] flex-col overflow-hidden" onCloseAutoFocus={(event) => { if (importOpener.current?.isConnected) { event.preventDefault(); importOpener.current.focus(); } }}>
        <div className="min-h-0 min-w-0 space-y-4 overflow-y-auto">
          <DialogHeader><DialogTitle className="pr-5 leading-snug">{t("siteImport.title")}</DialogTitle><DialogDescription className="text-xs leading-relaxed">{t("siteImport.hint")}</DialogDescription></DialogHeader>
          {!isTauri && <p className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t("siteFiles.preview")}</p>}
          <div className="space-y-2">
            <p className="text-xs font-medium">{t("siteImport.source")}</p>
            <p className="text-xs leading-relaxed text-muted [overflow-wrap:anywhere]">{importPath || t("siteImport.empty")}</p>
            <div className="flex flex-wrap gap-2"><Button size="sm" variant="secondary" disabled={locked} onClick={() => void run(async (operationId) => {
              let picked: string | null = "C:/Demo/backups/sample-site.zip";
              if (isTauri) { const { open } = await import("@tauri-apps/plugin-dialog"); const result = await open({ directory: false, multiple: false, filters: [{ name: "NiceEnv ZIP", extensions: ["zip"] }] }); picked = typeof result === "string" ? result : null; }
              if (!picked) return;
              setImportPath(picked); setImportPreview(null); setImportConfirmed(false);
              setImportPreview(await api.siteFilesInspectImport(siteId, picked, operationId));
            })}>{t(isTauri ? "siteImport.choose" : "siteImport.sample")}</Button>
              {importPath && <Button size="sm" variant="ghost" disabled={locked} onClick={() => void run(async (operationId) => { setImportPreview(null); setImportConfirmed(false); setImportPreview(await api.siteFilesInspectImport(siteId, importPath, operationId)); })}>{t("siteFiles.retry")}</Button>}
            </div>
          </div>
          {importPreview && <>
            <div className="space-y-2 rounded-lg bg-fill p-3 text-xs [overflow-wrap:anywhere]">
              <p className="font-mono">{importPreview.archive.name}</p><p className="font-mono text-muted">{importPreview.archive.root}</p>
              <p>{t("siteFiles.files").replace("{count}", String(importPreview.archive.files))} · {t("siteFiles.original")} {fmtBytes(importPreview.archive.originalBytes)} · {t("siteFiles.zip")} {fmtBytes(importPreview.archive.sizeBytes)}</p>
              {importPreview.archive.createdAt > 0 && <p>{new Date(importPreview.archive.createdAt).toLocaleString()}</p>}
              <p>{t("siteFiles.excluded")}: {importPreview.archive.excluded.join(", ") || t("siteFiles.noExclusions")}</p>
            </div>
            <div className="space-y-2 border-t border-dashed border-separator pt-4 text-xs [overflow-wrap:anywhere]">
              <p className="font-medium">{t("siteImport.target")} · {importPreview.targetName}</p><p className="font-mono text-muted">{importPreview.targetRoot}</p>
              <p className="leading-relaxed text-muted">{t("siteImport.checkHint")}</p>
            </div>
            <label className="flex items-start gap-2.5 text-xs leading-relaxed"><input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={importConfirmed} disabled={locked} onChange={(event) => setImportConfirmed(event.target.checked)} /><span>{t("siteImport.confirm")}</span></label>
          </>}
          {progressView}{errorView}
        </div>
        <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-4">
          <Button variant="ghost" disabled={busy} onClick={() => { setImportOpen(false); setError(null); }}>{t("common.cancel")}</Button>
          <Button className="h-auto min-h-9 whitespace-normal" disabled={locked || !importPreview || !importConfirmed} onClick={() => importPreview && void run(async (operationId) => {
            const next = await api.siteFilesImport(siteId, importPreview.sourcePath, importPreview.revision, importConfirmed, operationId);
            await client.cancelQueries({ queryKey }); client.setQueryData<api.SiteFileBackup[]>(queryKey, (previous) => [next, ...(previous ?? []).filter((item) => item.name !== next.name)]);
            setSearch(""); setPage(1); setImportOpen(false); setImportPreview(null); setImportConfirmed(false); toast.success(t("siteImport.done"));
          })}>{t("siteImport.submit")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    <Dialog open={!!restore} onOpenChange={(open) => { if (!open && !busyRef.current) { setRestore(null); setError(null); } }}>
      <DialogContent hideClose={busy} className="flex max-h-[85dvh] flex-col overflow-hidden" onCloseAutoFocus={(event) => { if (restoreOpener.current?.isConnected) { event.preventDefault(); restoreOpener.current.focus(); } }}>
        <div className="min-h-0 min-w-0 space-y-4 overflow-y-auto">
          <DialogHeader><DialogTitle className="pr-5 leading-snug">{t("siteFiles.restore")}</DialogTitle><DialogDescription className="text-xs leading-relaxed">{t("siteFiles.restoreHint")}</DialogDescription></DialogHeader>
          {restore && <div className="space-y-2 rounded-lg bg-fill p-3 text-xs [overflow-wrap:anywhere]"><p className="font-mono">{restore.name}</p><p className="font-mono text-muted">{restore.root}</p><p>{t("siteFiles.excluded")}: {restore.excluded.join(", ") || t("siteFiles.noExclusions")}</p></div>}
          <div className="space-y-2"><p className="text-xs font-medium">{t("siteFiles.parent")}</p><p className="font-mono text-xs [overflow-wrap:anywhere]">{parent || t("siteFiles.defaultParent")}</p>
            <div className="flex flex-wrap gap-2"><Button size="sm" variant="secondary" disabled={locked || !isTauri} onClick={async () => {
              if (busyRef.current || planBusyRef.current || disabled || dirty) return;
              setWorking(true);
              try { const { open } = await import("@tauri-apps/plugin-dialog"); const picked = await open({ directory: true }); if (typeof picked === "string") setParent(picked); }
              catch (failure) { setError(normalizeError(failure).message); }
              finally { setWorking(false); }
            }}>{t("siteFiles.choose")}</Button>{parent && <Button size="sm" variant="ghost" disabled={locked} onClick={() => setParent(null)}>{t("siteFiles.default")}</Button>}</div>
            {!isTauri && <p className="text-xs text-muted">{t("siteFiles.desktopPicker")}</p>}
          </div>
          <label className="flex items-start gap-2.5 border-t border-dashed border-separator pt-4 text-xs leading-relaxed"><input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={trusted} disabled={locked} onChange={(event) => setTrusted(event.target.checked)} /><span>{t("siteFiles.trust")}</span></label>
          {progressView}{errorView}
        </div>
        <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-4">
          <Button variant="ghost" disabled={busy} onClick={() => { setRestore(null); setError(null); }}>{t("common.cancel")}</Button>
          <Button className="h-auto min-h-9 whitespace-normal" disabled={locked || !trusted} onClick={() => restore && void run(async (operationId) => {
            const path = await api.siteFilesRestore(siteId, restore.name, parent, trusted, operationId);
            setRestored(path); setRestore(null); toast.success(t("siteFiles.restored"));
          })}>{t("siteFiles.restore")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={!!deleting} onOpenChange={(open) => { if (!open && !busyRef.current) { setDeleting(null); setError(null); } }}
      title={t("siteFiles.deleteTitle")} description={t("siteFiles.deleteHint")} confirmText={t("common.delete")} danger loading={busy} confirmDisabled={locked}
      onCloseAutoFocus={(event) => { if (deleteOpener.current?.isConnected) { event.preventDefault(); deleteOpener.current.focus(); } }}
      onConfirm={() => deleting && void run(async () => { await api.siteFilesDelete(siteId, deleting.name); await client.cancelQueries({ queryKey }); client.setQueryData<api.SiteFileBackup[]>(queryKey, (previous) => previous?.filter((item) => item.name !== deleting.name)); setDeleting(null); toast.success(t("siteFiles.deleted")); })}>
      <p className="font-mono text-xs [overflow-wrap:anywhere]">{deleting?.name}</p>{errorView}
    </ConfirmDialog>
  </div>;
}
