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
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import { listen } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
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
export function DbBackupCard() {
  const t = useT();
  const invalidate = useInvalidate();
  const [files, setFiles] = React.useState<DbBackupFile[]>([]);
  const [dir, setDir] = React.useState("");
  const [dbs, setDbs] = React.useState<string[]>([]);
  const [picked, setPicked] = React.useState<Set<string>>(new Set());
  const [busy, setBusy] = React.useState(false);
  const [progress, setProgress] = React.useState<DbBackupProgress | null>(null);
  const [dumpOpen, setDumpOpen] = React.useState(false);
  const [confirmRestore, setConfirmRestore] = React.useState<DbBackupFile | null>(null);
  const [confirmDelete, setConfirmDelete] = React.useState<DbBackupFile | null>(null);

  const systemDbs = React.useMemo(
    () => new Set(["mysql", "sys", "information_schema", "performance_schema"]),
    []
  );

  const load = React.useCallback(async () => {
    try {
      const [list, d, all] = await Promise.all([
        api.dbBackupList(),
        api.dbBackupDir(),
        api.dbList().catch(() => []),
      ]);
      setFiles(list);
      setDir(d);
      setDbs(all.map((x) => x.name).filter((n) => !systemDbs.has(n)));
    } catch {
      /* 服务没跑时不必报错，列表留空即可 */
    }
  }, [systemDbs]);

  React.useEffect(() => {
    void load();
  }, [load]);

  // 进度事件：导出/还原是长任务，没有这个用户会以为卡死
  React.useEffect(() => {
    let un: (() => void) | undefined;
    void listen<DbBackupProgress>("db://backup", (p) => {
      setProgress(p);
      if (p.state === "done") {
        window.setTimeout(() => setProgress(null), 1200);
      }
    }).then((u) => (un = u));
    return () => un?.();
  }, []);

  const toggle = (name: string) => {
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(name)) n.delete(name);
      else n.add(name);
      return n;
    });
  };

  const doDump = async () => {
    if (picked.size === 0) return;
    setBusy(true);
    setDumpOpen(false);
    try {
      const path = await api.dbBackupDump(Array.from(picked));
      toast.success(t("dbBackup.done"), { description: path.split(/[\\/]/).pop() });
      setPicked(new Set());
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const doRestore = async (f: DbBackupFile) => {
    setBusy(true);
    try {
      const r = await api.dbBackupRestore(f.path, true);
      toast.success(t("dbBackup.restored"), {
        description: r.safetyBackup
          ? t("dbBackup.safetyAt").replace("{p}", r.safetyBackup.split(/[\\/]/).pop() ?? "")
          : undefined,
      });
      invalidate("dbs");
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const doDelete = async (f: DbBackupFile) => {
    try {
      await api.dbBackupDelete(f.path);
      toast.success(t("dbBackup.deleted"));
      await load();
    } catch (e) {
      toastError(e);
    }
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
        <CardHeader className="flex-row items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2 text-[13px]">
              <DatabaseBackup className="h-3.5 w-3.5 text-primary" /> {t("dbBackup.title")}
            </CardTitle>
            <CardDescription className="mt-0.5">{t("dbBackup.subtitle")}</CardDescription>
          </div>
          <div className="flex items-center gap-1.5">
            <Button variant="ghost" size="sm" onClick={() => void openDir()} title={t("dbBackup.openDir")}>
              <FolderOpen className="h-3.5 w-3.5" />
            </Button>
            <Button size="sm" onClick={() => setDumpOpen(true)} disabled={busy || dbs.length === 0}>
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

          {files.length === 0 ? (
            <p className="py-6 text-center text-[12.5px] text-faint">{t("dbBackup.empty")}</p>
          ) : (
            <div className="space-y-1.5">
              {files.slice(0, 8).map((f) => (
                <div
                  key={f.path}
                  className="group flex items-center gap-2.5 rounded-lg border border-border/60 bg-card-2/25 px-2.5 py-2"
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
                  <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2"
                      onClick={() => setConfirmRestore(f)}
                      disabled={busy}
                      title={t("dbBackup.restore")}
                    >
                      <ArchiveRestore className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-error hover:text-error"
                      onClick={() => setConfirmDelete(f)}
                      disabled={busy}
                      title={t("dbBackup.delete")}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
              ))}
              {files.length > 8 && (
                <p className="pt-1 text-center text-[11px] text-faint">
                  {t("dbBackup.more").replace("{n}", String(files.length - 8))}
                </p>
              )}
            </div>
          )}
        </CardContent>
      </Card>

      {/* 选择要备份的库 */}
      <Dialog open={dumpOpen} onOpenChange={setDumpOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 text-[15px]">
              <Download className="h-4 w-4 text-primary" /> {t("dbBackup.pickDbs")}
            </DialogTitle>
            <DialogDescription>
              {t("dbBackup.pickHint")}
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
                  checked={picked.has(name)}
                  onChange={() => toggle(name)}
                />
                <span className="font-mono text-[12.5px]">{name}</span>
              </label>
            ))}
            {dbs.length === 0 && (
              <p className="py-6 text-center text-[12.5px] text-faint">{t("dbBackup.noDbs")}</p>
            )}
          </div>
          <div className="mt-1 flex items-center justify-between">
            <Button
              variant="ghost"
              size="sm"
              onClick={() =>
                setPicked(picked.size === dbs.length ? new Set() : new Set(dbs))
              }
            >
              {picked.size === dbs.length ? t("common.cancel") : t("dbBackup.selectAll")}
            </Button>
            <Button size="sm" onClick={() => void doDump()} disabled={picked.size === 0}>
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
        onOpenChange={(v) => !v && setConfirmRestore(null)}
        title={t("dbBackup.restoreTitle")}
        description={t("dbBackup.restoreDesc").replace("{n}", confirmRestore?.name ?? "")}
        confirmText={t("dbBackup.restore")}
        onConfirm={() => {
          const f = confirmRestore;
          setConfirmRestore(null);
          if (f) void doRestore(f);
        }}
      />

      <ConfirmDialog
        open={confirmDelete != null}
        onOpenChange={(v) => !v && setConfirmDelete(null)}
        title={t("dbBackup.deleteTitle")}
        description={t("dbBackup.deleteDesc").replace("{n}", confirmDelete?.name ?? "")}
        confirmText={t("dbBackup.delete")}
        danger
        onConfirm={() => {
          const f = confirmDelete;
          setConfirmDelete(null);
          if (f) void doDelete(f);
        }}
      />
    </>
  );
}
