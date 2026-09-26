"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  ArchiveRestore,
  DatabaseBackup,
  FolderOpen,
  Loader2,
  Trash2,
  Download,
  Upload,
  HardDriveDownload,
} from "lucide-react";
import type { DbBackupFile, DbBackupProgress } from "@nsb/schema";
import { useQuery } from "@tanstack/react-query";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import { listen } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog } from "@/components/shared/misc";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtTime(sec: number): string {
  if (!sec) return "—";
  const d = new Date(sec * 1000);
  const p = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * 数据库备份 / 还原。
 *
 * phpStudy 的招牌能力。这里除了「导出 / 导入」，多做三件事：
 * 1. **还原前自动兜底备份**——还原不可撤销，选错了得有退路；
 * 2. **进度可见**——大库导出要一会儿，界面显示已写出多少，不是干等；
 * 3. **只列本应用备份目录里的文件**——不做成「任意路径导入」，
 *    避免变成一个能读任意文件的接口（真要外部文件走导入按钮选）。
 */
export function DbBackupCard({ version, targetLabel, ready, databases: dbs, onLockChange }: {
  version: string; targetLabel: string; ready: boolean; databases: string[]; onLockChange: (locked: boolean) => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const query = useQuery({ queryKey: ["db-backups"], queryFn: api.dbBackupList, retry: false });
  const directory = useQuery({ queryKey: ["db-backup-dir"], queryFn: api.dbBackupDir, retry: false });
  const files = query.data ?? [];
  const dir = directory.data ?? "";
  const busyRef = React.useRef(false);
  const restoreOpener = React.useRef<HTMLButtonElement | null>(null);
  const [error, setError] = React.useState("");
  const [picked, setPicked] = React.useState<Set<string>>(new Set());
  const [busy, setBusy] = React.useState(false);
  const [progress, setProgress] = React.useState<DbBackupProgress | null>(null);
  const [dumpOpen, setDumpOpen] = React.useState(false);
  const [confirmRestore, setConfirmRestore] = React.useState<Pick<DbBackupFile, "path" | "name"> | null>(null);
  const [restoreDatabase, setRestoreDatabase] = React.useState("file");
  const [confirmDelete, setConfirmDelete] = React.useState<DbBackupFile | null>(null);

  const load = () => invalidate("db-backups");
  React.useEffect(() => {
    onLockChange(busy || dumpOpen || !!confirmRestore || !!confirmDelete);
    return () => onLockChange(false);
  }, [busy, dumpOpen, confirmRestore, confirmDelete, onLockChange]);

  React.useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<DbBackupProgress>("db://backup", (progress) => {
      if (busyRef.current) setProgress(progress);
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch(toastError);
    return () => { disposed = true; unlisten?.(); };
  }, []);

  const begin = () => {
    if (busyRef.current) return false;
    busyRef.current = true; setBusy(true); setError(""); setProgress(null); return true;
  };
  const end = () => { busyRef.current = false; setBusy(false); setProgress(null); };
  const fail = (error: unknown, notify = false) => { setError(normalizeError(error).message); if (notify) toastError(error); };
  const toggle = (name: string) => {
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(name)) n.delete(name);
      else n.add(name);
      return n;
    });
  };

  const doDump = async () => {
    if (!ready || picked.size === 0 || !begin()) return;
    try {
      const path = await api.dbBackupDump(Array.from(picked), undefined, version);
      toast.success(t("dbBackup.done"), { description: path.split(/[\\/]/).pop() });
      setDumpOpen(false);
      setPicked(new Set());
      await load();
    } catch (e) {
      fail(e);
    } finally {
      end();
    }
  };

  const pickSql = async () => {
    if (!ready || !begin()) return;
    let selected = false;
    try {
      if (!isTauri) { toast.info(t("dbBackup.desktopOnly")); return; }
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ title: t("dbBackup.importSql"), multiple: false, directory: false, filters: [{ name: "SQL", extensions: ["sql"] }] });
      if (typeof path === "string") {
        selected = true;
        setRestoreDatabase("");
        setConfirmRestore({ path, name: path.split(/[\\/]/).pop() ?? path });
      }
    } catch (error) { fail(error, true); }
    finally {
      end();
      if (!selected) requestAnimationFrame(() => restoreOpener.current?.focus());
    }
  };

  const doRestore = async (f: Pick<DbBackupFile, "path" | "name">) => {
    if (!ready || !restoreDatabase || (restoreDatabase !== "file" && !dbs.includes(restoreDatabase.slice(3))) || !begin()) return;
    try {
      const r = await api.dbBackupRestore(f.path, true, version, restoreDatabase === "file" ? undefined : restoreDatabase.slice(3));
      toast.success(t("dbBackup.restored"), {
        description: r.safetyBackup
          ? t("dbBackup.safetyAt").replace("{p}", r.safetyBackup.split(/[\\/]/).pop() ?? "")
          : undefined,
      });
      setConfirmRestore(null);
      invalidate("databases", "db-users");
      await load();
    } catch (e) {
      fail(e);
    } finally {
      invalidate("databases", "db-users", "db-backups");
      end();
    }
  };

  const doDelete = async (f: DbBackupFile) => {
    if (!begin()) return;
    try {
      await api.dbBackupDelete(f.path);
      toast.success(t("dbBackup.deleted"));
      setConfirmDelete(null);
      await load();
    } catch (e) {
      fail(e);
    } finally { end(); }
  };

  const openDir = async () => {
    try {
      await api.openInFolder(dir);
    } catch (e) {
      toastError(e);
    }
  };

  const pct =
    progress?.total && progress.total > 0
      ? Math.min(100, (progress.bytes / progress.total) * 100)
      : null;

  return (
    <>
      <Card>
        <CardHeader className="flex-row flex-wrap items-center justify-between gap-3">
          <div>
            <CardTitle className="flex items-center gap-2 text-[13px]">
              <DatabaseBackup className="h-3.5 w-3.5 text-primary" /> {t("dbBackup.title")}
            </CardTitle>
            <CardDescription className="mt-0.5">{t("dbBackup.subtitle")}</CardDescription>
          </div>
          <div className="flex flex-wrap items-center gap-1.5">
            <Button variant="ghost" size="sm" onClick={() => void openDir()} disabled={!dir} aria-label={t("dbBackup.openDir")} title={t("dbBackup.openDir")}>
              <FolderOpen className="h-3.5 w-3.5" />
            </Button>
            <Button variant="secondary" size="sm" disabled={!ready || busy} onClick={(event) => { restoreOpener.current = event.currentTarget; void pickSql(); }}>
              <Upload className="h-3.5 w-3.5" /> {t("dbBackup.importSql")}
            </Button>
            <Button size="sm" onClick={() => { setError(""); setPicked(new Set()); setDumpOpen(true); }} disabled={!ready || busy || dbs.length === 0}>
              <HardDriveDownload className="h-3.5 w-3.5" /> {t("dbBackup.new")}
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          {/* 进度条：只在任务进行时出现，不占常驻空间 */}
          {progress && (
            <div className="mb-3 rounded-lg border border-info/25 bg-info-soft px-3 py-2">
              <div className="flex items-center gap-2 text-[11.5px]">
                <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-info" />
                <span className="truncate font-mono text-info">{progress.database}</span>
                <span className="ml-auto shrink-0 tabular text-faint">
                  {fmtBytes(progress.bytes)}
                  {pct != null ? ` · ${pct.toFixed(0)}%` : ""}
                </span>
              </div>
              {pct != null && (
                <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-card-2">
                  <div
                    className="h-full rounded-full bg-info transition-[width] duration-200"
                    style={{ width: `${pct}%` }}
                  />
                </div>
              )}
            </div>
          )}

          {query.isPending ? <p role="status" className="py-6 text-sm text-muted">{t("db.loading")}</p> : query.isError ? <p role="alert" className="py-4 text-sm text-error">{t("dbBackup.loadFailed")} <Button variant="ghost" onClick={() => void query.refetch()}>{t("db.retry")}</Button></p> : files.length === 0 ? (
            <p className="py-6 text-center text-[12.5px] text-faint">{t("dbBackup.empty")}</p>
          ) : (
            <div className="max-h-80 space-y-1.5 overflow-y-auto">
              {files.map((f) => (
                <div
                  key={f.path}
                  className="group flex flex-wrap items-center gap-2.5 rounded-lg border border-border/60 bg-card-2/25 px-2.5 py-2"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-mono text-[11.5px]">{f.name}</span>
                      <Badge variant="outline" className="shrink-0 text-[9.5px]">
                        {fmtBytes(f.sizeBytes)}
                      </Badge>
                    </div>
                    <p className="text-[10.5px] text-faint">{fmtTime(f.createdAt)}</p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2"
                      onClick={(event) => { restoreOpener.current = event.currentTarget; setError(""); setRestoreDatabase("file"); setConfirmRestore(f); }}
                      disabled={busy || !ready}
                      aria-label={`${t("dbBackup.restore")} ${f.name}`}
                      title={t("dbBackup.restore")}
                    >
                      <ArchiveRestore className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-error hover:text-error"
                      onClick={() => { setError(""); setConfirmDelete(f); }}
                      aria-label={`${t("dbBackup.delete")} ${f.name}`}
                      disabled={busy}
                      title={t("dbBackup.delete")}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
              ))}

            </div>
          )}
        </CardContent>
      </Card>

      {/* 选择要备份的库 */}
      <Dialog open={dumpOpen} onOpenChange={(value) => !busy && setDumpOpen(value)}>
        <DialogContent hideClose={busy} className="max-w-md max-h-[85dvh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 text-[15px]">
              <Download className="h-4 w-4 text-primary" /> {t("dbBackup.pickDbs")}
            </DialogTitle>
            <DialogDescription>
              {targetLabel} · {t("dbBackup.pickHint")}
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-72 space-y-1 overflow-y-auto">
            {dbs.map((name) => (
              <label
                key={name}
                className={cn(
                  "flex cursor-pointer items-center gap-2.5 rounded-lg border px-3 py-2 transition-colors",
                  picked.has(name) ? "border-primary/40 bg-primary-soft" : "border-border/60"
                )}
              >
                <input
                  type="checkbox"
                  className="h-3.5 w-3.5 shrink-0 accent-[var(--primary)]"
                  disabled={busy}
                  checked={picked.has(name)}
                  onChange={() => toggle(name)}
                />
                <span className="min-w-0 break-all font-mono text-[12.5px]">{name}</span>
              </label>
            ))}
            {dbs.length === 0 && (
              <p className="py-6 text-center text-[12.5px] text-faint">{t("dbBackup.noDbs")}</p>
            )}
          </div>
          {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
          {busy && <p role="status" className="text-sm text-muted">{progress?.message ?? t("confirm.busy")}{progress?.bytes ? ` · ${fmtBytes(progress.bytes)}` : ""}</p>}
          <div className="mt-1 flex flex-wrap items-center justify-between gap-2">
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={() =>
                setPicked(picked.size === dbs.length ? new Set() : new Set(dbs))
              }
            >
              {picked.size === dbs.length ? t("common.cancel") : t("dbBackup.selectAll")}
            </Button>
            <Button size="sm" onClick={() => void doDump()} disabled={busy || picked.size === 0 || !ready}>
              <Upload className="h-3.5 w-3.5" />
              <span className="ml-1.5">
                {t("dbBackup.exportN").replace("{n}", String(picked.size))}
              </span>
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={confirmRestore != null}
        onCloseAutoFocus={(event) => {
          if (restoreOpener.current?.isConnected) { event.preventDefault(); restoreOpener.current.focus(); }
        }}
        onOpenChange={(v) => !v && !busy && setConfirmRestore(null)}
        title={t("dbBackup.restoreTitle")}
        description={`${targetLabel} · ${t("dbBackup.restoreDesc").replace("{n}", confirmRestore?.name ?? "")}`}
        confirmText={t("dbBackup.restore")}
        loading={busy}
        confirmDisabled={!ready || !restoreDatabase || (restoreDatabase !== "file" && !dbs.includes(restoreDatabase.slice(3)))}
        danger
        onConfirm={() => { if (confirmRestore) void doRestore(confirmRestore); }}
      >
        <p className="text-sm text-muted">{t("dbBackup.targetHint")}</p>
        <div className="space-y-1.5">
          <Label htmlFor="restore-database">{t("dbBackup.defaultDatabase")}</Label>
          <Select value={restoreDatabase} disabled={busy} onValueChange={setRestoreDatabase}>
            <SelectTrigger id="restore-database"><SelectValue placeholder={t("dbBackup.chooseScope")} /></SelectTrigger>
            <SelectContent><SelectItem value="file">{t("dbBackup.databaseFromFile")}</SelectItem>{dbs.map((db) => <SelectItem key={db} value={`db:${db}`}>{db}</SelectItem>)}</SelectContent>
          </Select>
        </div>
        <p className="text-xs text-muted">{t("dbBackup.sqlScope")}</p>
        <p className="break-all font-mono text-xs text-muted">{confirmRestore?.path}</p>
        {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
        {busy && <p role="status" className="text-sm text-muted">{progress?.message ?? t("confirm.busy")}</p>}
      </ConfirmDialog>

      <ConfirmDialog
        open={confirmDelete != null}
        onOpenChange={(v) => !v && !busy && setConfirmDelete(null)}
        title={t("dbBackup.deleteTitle")}
        description={t("dbBackup.deleteDesc").replace("{n}", confirmDelete?.name ?? "")}
        confirmText={t("dbBackup.delete")}
        danger
        loading={busy}
        onConfirm={() => { if (confirmDelete) void doDelete(confirmDelete); }}
      >{error && <p role="alert" className="break-words text-sm text-error">{error}</p>}</ConfirmDialog>
    </>
  );
}
