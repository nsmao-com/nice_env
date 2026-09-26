"use client";

import * as React from "react";
import { toast } from "sonner";
import { CheckSquare, Loader2, Play, Square, X } from "lucide-react";
import type { Site } from "@nsb/schema";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

/**
 * 批量启停站点。
 *
 * 启动逐项校验依赖与结果；停止统一重载配置。失败项保留，便于修复后重试。
 */
export function SiteBulkActions({ sites }: { sites: Site[] }) {
  const t = useT();
  const invalidate = useInvalidate();
  const [open, setOpen] = React.useState(false);
  const [picked, setPicked] = React.useState<Set<string>>(new Set());
  const [busy, setBusy] = React.useState(false);

  const running = React.useMemo(
    () => sites.filter((s) => s.status === "running").map((s) => s.id),
    [sites]
  );
  const stopped = React.useMemo(
    () => sites.filter((s) => s.status !== "running").map((s) => s.id),
    [sites]
  );

  React.useEffect(() => {
    if (!open) setPicked(new Set());
  }, [open]);

  const toggle = (id: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const run = async (action: "start" | "stop") => {
    const ids = Array.from(picked);
    if (ids.length === 0 || busy) return;
    setBusy(true);
    try {
      const r = action === "start" ? await api.sitesStartMany(ids) : await api.sitesStopMany(ids);
      invalidate("sites", "services", "hosts");
      if (r.failed.length === 0) {
        toast.success(
          t("siteBulk.done")
            .replace("{action}", t(`siteBulk.${action}`))
            .replace("{n}", String(r.succeeded.length))
        );
        setOpen(false);
      } else {
        toast.warning(
          t("siteBulk.partial")
            .replace("{ok}", String(r.succeeded.length))
            .replace("{fail}", String(r.failed.length)),
          {
            description: r.failed
              .slice(0, 3)
              .map((f) => `${f.siteId}: ${f.error.message}`)
              .join("\n"),
            duration: 10000,
          }
        );
      }
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  if (sites.length === 0) return null;

  return (
    <>
      <Button variant="secondary" size="sm" onClick={() => setOpen(true)} title={t("siteBulk.title")}>
        <CheckSquare className="h-3.5 w-3.5" />
        <span className="hidden sm:inline">{t("siteBulk.title")}</span>
      </Button>

      <Dialog open={open} onOpenChange={(value) => !busy && setOpen(value)}>
        <DialogContent className="flex max-h-[80vh] max-w-xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 border-b border-border px-5 py-4">
            <DialogTitle className="text-[15px]">{t("siteBulk.title")}</DialogTitle>
            <DialogDescription className="mt-0.5 text-[11.5px]">
              {t("siteBulk.subtitle").replace("{n}", String(picked.size))}
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
            <div className="space-y-1">
              {sites.map((s) => (
                <label
                  key={s.id}
                  className={cn(
                    "flex cursor-pointer items-center gap-3 rounded-lg border px-3 py-2 transition-colors",
                    picked.has(s.id)
                      ? "border-primary/40 bg-primary-soft"
                      : "border-border/60 hover:border-border-strong"
                  )}
                >
                  <input
                    type="checkbox"
                    className="h-3.5 w-3.5 shrink-0 accent-[var(--primary)]"
                    checked={picked.has(s.id)}
                    onChange={() => toggle(s.id)}
                  />
                  <span className="min-w-0 flex-1 truncate text-[12.5px]">{s.name}</span>
                  <span className="shrink-0 truncate font-mono text-[10.5px] text-faint">
                    {s.domains[0]}
                  </span>
                  <span
                    className={cn(
                      "shrink-0 text-[10.5px]",
                      s.status === "running" ? "text-running" : "text-faint"
                    )}
                  >
                    {s.status === "running" ? t("state.running") : t("state.stopped")}
                  </span>
                </label>
              ))}
            </div>
          </div>

          <div className="flex shrink-0 items-center justify-between gap-2 border-t border-border px-5 py-3">
            <div className="flex items-center gap-1.5">
              <Button
                variant="ghost"
                size="sm"
                className="h-7 text-[11.5px]"
                onClick={() => setPicked(new Set(running))}
                disabled={running.length === 0}
              >
                {t("bulk.selectRunning")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="h-7 text-[11.5px]"
                onClick={() => setPicked(new Set(stopped))}
                disabled={stopped.length === 0}
              >
                {t("bulk.selectStopped")}
              </Button>
              {picked.size > 0 && (
                <Button variant="ghost" size="sm" className="h-7" onClick={() => setPicked(new Set())}>
                  <X className="h-3 w-3" />
                </Button>
              )}
            </div>
            <div className="flex items-center gap-1.5">
              <Button
                size="sm"
                variant="secondary"
                className="h-8"
                disabled={busy || picked.size === 0}
                onClick={() => void run("stop")}
              >
                {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Square className="h-3.5 w-3.5" />}
                <span className="ml-1.5">{t("bulk.stop")}</span>
              </Button>
              <Button size="sm" className="h-8" disabled={busy || picked.size === 0} onClick={() => void run("start")}>
                {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Play className="h-3.5 w-3.5" />}
                <span className="ml-1.5">{t("bulk.start")}</span>
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
