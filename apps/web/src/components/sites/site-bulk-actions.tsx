"use client";

import * as React from "react";
import { CheckSquare, Loader2, Play, Square, X } from "lucide-react";
import type { Site, SiteBulkReport } from "@nsb/schema";
import { useT } from "@/lib/store";
import { useInvalidate } from "@/lib/hooks";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
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
  const [busy, setBusy] = React.useState<"start" | "stop" | null>(null);
  const busyRef = React.useRef(false);
  const [report, setReport] = React.useState<SiteBulkReport | null>(null);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [requested, setRequested] = React.useState<{ id: string; name: string }[]>([]);
  const resultRef = React.useRef<HTMLDivElement>(null);
  const retryAction = report?.action === "start" || report?.action === "stop" ? report.action : null;

  const running = React.useMemo(
    () => sites.filter((s) => s.status === "running").map((s) => s.id),
    [sites]
  );
  const stopped = React.useMemo(
    () => sites.filter((s) => s.status !== "running").map((s) => s.id),
    [sites]
  );

  React.useEffect(() => {
    if (!open) { setPicked(new Set()); setReport(null); setError(null); setRequested([]); }
  }, [open]);

  React.useEffect(() => {
    if (!busy && (report || error)) resultRef.current?.focus();
  }, [busy, report, error]);

  const toggle = (id: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const run = async (action: "start" | "stop", ids = Array.from(picked)) => {
    if (ids.length === 0 || busyRef.current) return;
    busyRef.current = true;
    setBusy(action);
    setReport(null);
    setError(null);
    setRequested(ids.map((id) => ({id, name: sites.find((site) => site.id === id)?.name ?? requested.find((site) => site.id === id)?.name ?? id})));
    try {
      const r = action === "start" ? await api.sitesStartMany(ids) : await api.sitesStopMany(ids);
      setReport(r);
      setPicked(new Set(r.failed.map((failure) => failure.siteId)));
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      invalidate("sites", "services", "hosts");
      busyRef.current = false;
      setBusy(null);
    }
  };

  if (sites.length === 0 && !open) return null;

  return (
    <>
      <Button variant="secondary" size="sm" onClick={() => setOpen(true)} title={t("siteBulk.title")} aria-label={t("siteBulk.title")}>
        <CheckSquare className="h-3.5 w-3.5" />
        <span className="hidden sm:inline">{t("siteBulk.title")}</span>
      </Button>

      <Dialog open={open} onOpenChange={(value) => !busyRef.current && setOpen(value)}>
        <DialogContent hideClose={busy !== null} aria-busy={busy !== null} className="flex max-h-[85dvh] max-w-xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 px-4 py-4 pr-12 sm:px-5 sm:pr-12">
            <DialogTitle className="text-[15px]">{t("siteBulk.title")}</DialogTitle>
            <DialogDescription className="mt-0.5 text-[11.5px]">
              {t("siteBulk.subtitle").replace("{n}", String(picked.size))}
            </DialogDescription>
          </DialogHeader>
          <div className="mx-4 shrink-0 border-t border-dashed border-separator sm:mx-5" />

          <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4 sm:px-5">
            <p className="mb-3 text-xs leading-relaxed text-muted">{t("siteBulk.stopHint")}</p>
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
                    disabled={busy !== null}
                    aria-label={s.name}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[12.5px]" title={s.name}>{s.name}</span>
                    <span className="block truncate font-mono text-[10.5px] text-faint" title={s.domains.join(", ")}>{s.domains.join(", ")}</span>
                  </span>
                  <span
                    className={cn(
                      "shrink-0 text-[10.5px]",
                      s.status === "running" ? "text-running" : "text-faint"
                    )}
                  >
                    {s.status === "unconfigured" ? t("sites.unconfigured") : t(`state.${s.status}`)}
                  </span>
                </label>
              ))}
            </div>
            {(report || error) && <div ref={resultRef} tabIndex={-1} className="mt-4 min-w-0 space-y-2 rounded-xl border border-border/70 bg-card-2/30 p-3 outline-none focus-visible:ring-2 focus-visible:ring-primary/30 [overflow-wrap:anywhere]" aria-live="polite">
              {error && <div role="alert" className="text-xs text-error"><p>{error.message}</p>{error.hint && <p className="mt-1 text-muted">{error.hint}</p>}</div>}
              {report && <>
                <p className="text-xs font-medium">{t("bulk.resultSummary").replace("{ok}", String(report.succeeded.length)).replace("{already}", String(report.already.length)).replace("{fail}", String(report.failed.length))}</p>
                <ul className="space-y-3">
                  {requested.map((site) => {
                    const failure = report.failed.find((row) => row.siteId === site.id);
                    const state = failure ? "bulk.rFail" : report.succeeded.includes(site.id) ? "bulk.rOk" : report.already.includes(site.id) ? "bulk.rSkipped" : "bulk.rUnknown";
                    return <li key={site.id} className="min-w-0 text-xs">
                      <div className="flex items-start gap-2"><span className="min-w-0 flex-1">{site.name}</span><span className={cn("shrink-0", failure ? "text-error" : "text-muted")}>{t(state)}</span></div>
                      {failure && <div className="mt-1 space-y-1 text-error"><p>{failure.error.message}</p>{failure.error.hint && <p className="text-muted">{failure.error.hint}</p>}{failure.error.detail && <details className="text-muted"><summary className="cursor-pointer">{t("siteBulk.failureDetails")}</summary><p className="mt-1 whitespace-pre-wrap">{failure.error.detail}</p></details>}</div>}
                    </li>;
                  })}
                </ul>
                {retryAction && report.failed.length > 0 && <Button variant="secondary" size="sm" disabled={busy !== null} className="mt-1 h-auto min-h-8 whitespace-normal" onClick={() => void run(retryAction, report.failed.map((failure) => failure.siteId))}>{t("siteBulk.retryFailed")}</Button>}
              </>}
            </div>}
          </div>

          <div className="mx-4 flex shrink-0 flex-col gap-2 border-t border-dashed border-separator py-3 sm:mx-5 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex flex-wrap items-center gap-1.5">
              <Button
                variant="ghost"
                size="sm"
                className="h-7 text-[11.5px]"
                onClick={() => setPicked(new Set(running))}
                disabled={busy !== null || running.length === 0}
              >
                {t("bulk.selectRunning")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="h-7 text-[11.5px]"
                onClick={() => setPicked(new Set(stopped))}
                disabled={busy !== null || stopped.length === 0}
              >
                {t("bulk.selectStopped")}
              </Button>
              {picked.size > 0 && (
                <Button variant="ghost" size="sm" className="h-7" disabled={busy !== null} aria-label={t("siteBulk.clearSelection")} onClick={() => setPicked(new Set())}>
                  <X className="h-3 w-3" />
                </Button>
              )}
            </div>
            <div className="flex items-center justify-end gap-1.5">
              <Button
                size="sm"
                variant="secondary"
                className="h-8"
                disabled={busy !== null || picked.size === 0}
                onClick={() => void run("stop")}
              >
                {busy === "stop" ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Square className="h-3.5 w-3.5" />}
                <span className="ml-1.5">{t("bulk.stop")}</span>
              </Button>
              <Button size="sm" className="h-8" disabled={busy !== null || picked.size === 0} onClick={() => void run("start")}>
                {busy === "start" ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Play className="h-3.5 w-3.5" />}
                <span className="ml-1.5">{t("bulk.start")}</span>
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
