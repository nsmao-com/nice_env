"use client";

import * as React from "react";
import Link from "next/link";
import {
  CheckSquare,
  Loader2,
  Play,
  RotateCw,
  Square,
  X,
  ListChecks,
} from "lucide-react";
import type { ServiceStatus, BulkReport } from "@nsb/schema";
import { useT } from "@/lib/store";
import { useInvalidate, serviceHasProcess } from "@/lib/hooks";
import * as api from "@/lib/api";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
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
 * 批量服务操作。
 *
 * 「服务栈」适合固定组合；但日常更常见的是**临时**一批：调试时想只停掉
 * 数据相关的三个、或把上次崩掉的几个一起拉起来。为此专门存一个栈太重。
 *
 * 执行顺序不是照勾选顺序来的 —— 启动按依赖分层（数据层 → 运行时 → Web 服务器），
 * 停止反过来。否则 nginx 会在上游还没就绪时起来，直接 502。
 * 界面上会把实际执行顺序显示出来，让用户知道不是我们乱序了。
 */
export function BulkActions({ services }: { services: ServiceStatus[] }) {
  const t = useT();
  const invalidate = useInvalidate();
  const [open, setOpen] = React.useState(false);
  const [picked, setPicked] = React.useState<Set<string>>(new Set());
  const [busy, setBusy] = React.useState<string | null>(null);
  const [report, setReport] = React.useState<BulkReport | null>(null);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const busyRef = React.useRef(false);
  const resultRef = React.useRef<HTMLDivElement>(null);

  const running = React.useMemo(
    () => services.filter(serviceHasProcess).map((s) => s.id),
    [services]
  );
  const stopped = React.useMemo(
    () => services.filter((s) => !serviceHasProcess(s)).map((s) => s.id),
    [services]
  );

  React.useEffect(() => {
    if (!open) {
      setReport(null);
      setError(null);
      setPicked(new Set());
    }
  }, [open]);

  const toggle = (id: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const run = async (action: "start" | "stop" | "restart", ids = Array.from(picked)) => {
    if (ids.length === 0 || busyRef.current) return;
    busyRef.current = true;
    setBusy(action);
    setError(null);
    try {
      const r =
        action === "start"
          ? await api.bulkStart(ids)
          : action === "stop"
            ? await api.bulkStop(ids)
            : await api.bulkRestart(ids);
      setReport(r);
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      busyRef.current = false;
      setBusy(null);
      invalidate("services", "stacks");
    }
  };

  React.useEffect(() => {
    if (!busy && (report || error)) resultRef.current?.scrollIntoView({ block: "nearest" });
  }, [busy, report, error]);

  // 与 service-card 保持同一套状态文案，避免两处叫法不一致
  const stateLabel: Record<string, string> = {
    running: t("state.running"),
    stopped: t("state.stopped"),
    error: t("state.error"),
    starting: t("state.starting"),
    stopping: t("state.stopping"),
    unknown: t("state.unknown"),
  };

  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        onClick={() => setOpen(true)}
        disabled={services.length === 0}
        title={t("bulk.title")}
        aria-label={t("bulk.title")}
      >
        <ListChecks className="h-3.5 w-3.5" />
        <span className="hidden sm:inline">{t("bulk.title")}</span>
      </Button>

      <Dialog open={open} onOpenChange={(next) => { if (!busyRef.current) setOpen(next); }}>
        <DialogContent hideClose={busy !== null} aria-busy={busy !== null} className="flex max-h-[85dvh] max-w-2xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 border-b border-border px-4 py-4 pr-10 sm:px-5">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-primary/30 bg-primary-soft">
                <CheckSquare className="h-[18px] w-[18px] text-primary" strokeWidth={1.8} />
              </div>
              <div className="min-w-0 flex-1">
                <DialogTitle className="text-[15px]">{t("bulk.title")}</DialogTitle>
                <DialogDescription className="mt-0.5 text-[11.5px]">
                  {t("bulk.subtitle").replace("{n}", String(picked.size))}
                </DialogDescription>
              </div>
            </div>
          </DialogHeader>

          <div className="min-h-0 flex-1 overflow-y-auto px-3 py-4 sm:px-5">
            <div className="space-y-1">
              {services.map((s) => (
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
                    aria-label={s.label}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
                      <span className="min-w-0 basis-full truncate text-[12.5px] font-medium sm:basis-auto">{s.label}</span>
                      {s.version && (
                        <span className="shrink-0 font-mono text-[10.5px] text-faint">{s.version}</span>
                      )}
                      <span
                        className={cn(
                          "ml-auto shrink-0 text-[10.5px]",
                          s.state === "running" ? "text-running" : "text-faint"
                        )}
                      >
                        {stateLabel[s.state] ?? s.state}
                      </span>
                    </div>
                  </div>
                </label>
              ))}
            </div>

            <div ref={resultRef}>
              <BulkResult report={report} error={error} services={services} busy={busy !== null} />
              {report && report.failed.length > 0 && (
                <Button variant="secondary" size="sm" className="mt-2" disabled={busy !== null}
                  onClick={() => void run(report.action as "start" | "stop" | "restart", report.failed.map((f) => f.serviceId))}>
                  <RotateCw className="h-3.5 w-3.5" /> {t("bulk.retryFailed")}
                </Button>
              )}
            </div>
          </div>

          <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-t border-border px-3 py-3 sm:px-5">
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
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 text-[11.5px]"
                  disabled={busy !== null}
                  onClick={() => setPicked(new Set())}
                >
                  <X className="h-3 w-3" />
                  <span className="ml-1">{t("bulk.clear")}</span>
                </Button>
              )}
            </div>

            <div className="flex flex-wrap items-center gap-1.5">
              <Button
                size="sm"
                variant="secondary"
                className="h-8"
                disabled={busy != null || picked.size === 0}
                onClick={() => void run("stop")}
              >
                {busy === "stop" ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Square className="h-3.5 w-3.5" />
                )}
                <span className="ml-1.5">{t("bulk.stop")}</span>
              </Button>
              <Button
                size="sm"
                variant="secondary"
                className="h-8"
                disabled={busy != null || picked.size === 0}
                onClick={() => void run("restart")}
              >
                {busy === "restart" ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RotateCw className="h-3.5 w-3.5" />
                )}
                <span className="ml-1.5">{t("bulk.restart")}</span>
              </Button>
              <Button
                size="sm"
                className="h-8"
                disabled={busy != null || picked.size === 0}
                onClick={() => void run("start")}
              >
                {busy === "start" ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Play className="h-3.5 w-3.5" />
                )}
                <span className="ml-1.5">{t("bulk.start")}</span>
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}

/** 所有批量入口使用相同的逐项结果，长错误换行且失败优先于成功标记。 */
export function BulkResult({ report, error, services, busy = false }: {
  report: BulkReport | null;
  error?: AppErrorShape | null;
  busy?: boolean;
  services: ServiceStatus[];
}) {
  const t = useT();
  return (
    <div className="min-w-0 space-y-2 [overflow-wrap:anywhere]" aria-live="polite">
      {error && <p role="alert" className="rounded-lg bg-error-soft p-3 text-xs text-error">{error.message}{error.hint && <span className="mt-1 block">{error.hint}</span>}</p>}
      {report && (
        <div className="mt-3 rounded-xl border border-border/70 bg-card-2/30 p-3">
          <p className="text-xs font-medium">
            {t("bulk.resultSummary").replace("{ok}", String(report.succeeded.length))
              .replace("{already}", String(report.already.length)).replace("{fail}", String(report.failed.length))}
          </p>
          <p className="mt-1 text-[10.5px] text-faint">{t("bulk.execOrder")}</p>
          {report.action === "restart" && <p className="mt-1 text-[11px] text-muted">{t("bulk.restartOrder")}</p>}
          <ol className="mt-2 space-y-2">
            {report.order.map((id, index) => {
              const failure = report.failed.find((f) => f.serviceId === id);
              const ok = !failure && report.succeeded.includes(id);
              const already = !failure && report.already.includes(id);
              return (
                <li key={id} className="min-w-0 text-xs">
                  <div className="flex items-start gap-2">
                    <span className="shrink-0 text-faint">{index + 1}.</span>
                    <span className="min-w-0 flex-1">{services.find((s) => s.id === id)?.label ?? id}</span>
                    <span className={cn("shrink-0", failure ? "text-error" : ok ? "text-running" : "text-faint")}>
                      {t(failure ? "bulk.rFail" : ok ? "bulk.rOk" : already ? "bulk.rSkipped" : "bulk.rUnknown")}
                    </span>
                  </div>
                  {failure && (
                    <div className="mt-1 pl-5 text-[11px] text-error">
                      <p>{failure.error.message}</p>
                      {failure.error.hint && <p className="mt-1 text-muted">{failure.error.hint}</p>}
                      <Link aria-disabled={busy} tabIndex={busy ? -1 : undefined} onClick={(event) => { if (busy) event.preventDefault(); }} className="mt-1 inline-block text-primary underline underline-offset-2 aria-disabled:opacity-50" href={`/logs?service=${encodeURIComponent(id)}`}>{t("logs.title")}</Link>
                    </div>
                  )}
                </li>
              );
            })}
          </ol>
        </div>
      )}
    </div>
  );
}
