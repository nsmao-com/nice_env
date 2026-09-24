"use client";

import * as React from "react";
import { toast } from "sonner";
import { Download, Loader2 } from "lucide-react";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/**
 * 从其它环境（FlyEnv / phpStudy / ServBay / XAMPP…）导入 MySQL 数据库：
 * 连源实例 → 枚举用户库 → 勾选 → mysqldump 管道导入本地托管实例。
 */
export function DbImportDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const [host, setHost] = React.useState("127.0.0.1");
  const [port, setPort] = React.useState("3306");
  const [user, setUser] = React.useState("root");
  const [password, setPassword] = React.useState("");
  const [probing, setProbing] = React.useState(false);
  const [importing, setImporting] = React.useState(false);
  const [dbs, setDbs] = React.useState<api.SourceDb[] | null>(null);
  const [selected, setSelected] = React.useState<Set<string>>(new Set());

  const probe = async () => {
    setProbing(true);
    setDbs(null);
    try {
      const list = await api.migrateListSource(host, Number(port) || 3306, user, password);
      setDbs(list);
      setSelected(new Set(list.map((d) => d.name)));
      if (list.length === 0) toast.info(t("dbImport.none"));
    } catch (e) {
      toastError(e);
    } finally {
      setProbing(false);
    }
  };

  const runImport = async () => {
    const dbsToImport = [...selected];
    if (dbsToImport.length === 0) {
      toast.info(t("dbImport.selectFirst"));
      return;
    }
    setImporting(true);
    try {
      const r = await api.migrateImport(host, Number(port) || 3306, user, password, dbsToImport);
      if (r.failed.length === 0) {
        toast.success(`${t("dbImport.doneP1")} ${r.imported.length} ${t("dbImport.doneP2")}`);
      } else {
        toast.warning(t("dbImport.partial"), {
          description: r.failed.map(([db, err]) => `${db}: ${err}`).join("\n"),
          duration: 10000,
        });
      }
      invalidate("databases");
      onOpenChange(false);
    } catch (e) {
      toastError(e);
    } finally {
      setImporting(false);
    }
  };

  const fmtSize = (kb?: number) => {
    if (kb == null) return "";
    if (kb < 1024) return `${kb} KB`;
    if (kb < 1024 * 1024) return `${(kb / 1024).toFixed(1)} MB`;
    return `${(kb / 1024 / 1024).toFixed(2)} GB`;
  };

  return (
    <Dialog open={open} onOpenChange={importing ? undefined : onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("dbImport.title")}</DialogTitle>
          <DialogDescription>{t("dbImport.desc")}</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-3">
          {/* 源连接 */}
          <div className="grid grid-cols-[1fr_5.5rem] gap-2">
            <Input
              value={host}
              onChange={(e) => setHost(e.target.value)}
              placeholder={t("dbImport.host")}
              className="font-mono"
            />
            <Input
              value={port}
              onChange={(e) => setPort(e.target.value)}
              placeholder="3306"
              className="font-mono"
              inputMode="numeric"
            />
          </div>
          <div className="grid grid-cols-[1fr_1fr_auto] items-center gap-2">
            <Input
              value={user}
              onChange={(e) => setUser(e.target.value)}
              placeholder={t("dbImport.user")}
              className="font-mono"
            />
            <Input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder={t("dbImport.password")}
              className="font-mono"
              onKeyDown={(e) => e.key === "Enter" && probe()}
            />
            <Button variant="secondary" disabled={probing} onClick={probe}>
              {probing ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
              {t("dbImport.detect")}
            </Button>
          </div>

          {/* 库列表 */}
          {dbs && (
            <div className="max-h-56 overflow-y-auto rounded-lg border border-border bg-card-2/30">
              {dbs.length === 0 ? (
                <p className="px-3 py-4 text-center text-[11px] text-faint">{t("dbImport.none")}</p>
              ) : (
                dbs.map((d) => (
                  <label
                    key={d.name}
                    className="flex cursor-pointer items-center gap-3 border-b border-border/50 px-3.5 py-2.5 last:border-0 hover:bg-card-2/40"
                  >
                    <input
                      type="checkbox"
                      checked={selected.has(d.name)}
                      onChange={(e) => {
                        const next = new Set(selected);
                        if (e.target.checked) next.add(d.name);
                        else next.delete(d.name);
                        setSelected(next);
                      }}
                      className="h-3.5 w-3.5 accent-[var(--apple-blue)]"
                    />
                    <code className="min-w-0 flex-1 truncate font-mono text-[12px]">{d.name}</code>
                    {d.sizeKb != null && d.sizeKb > 0 && (
                      <span className="shrink-0 text-[10.5px] text-faint">{fmtSize(d.sizeKb)}</span>
                    )}
                  </label>
                ))
              )}
            </div>
          )}

          {/* 动作 */}
          <div className="flex items-center justify-between gap-2">
            <p className="text-[10.5px] leading-snug text-faint">{t("dbImport.targetHint")}</p>
            <Button
              disabled={importing || !dbs || selected.size === 0}
              onClick={runImport}
            >
              {importing ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" />}
              {t("dbImport.run")}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
