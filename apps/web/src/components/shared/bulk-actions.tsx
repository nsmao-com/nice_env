"use client";

import * as React from "react";
import { toast } from "sonner";
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

  const running = React.useMemo(
    () => services.filter((s) => s.state === "running").map((s) => s.id),
    [services]
  );
  const stopped = React.useMemo(
    () => services.filter((s) => s.state !== "running").map((s) => s.id),
    [services]
  );

  React.useEffect(() => {
    if (!open) {
      setReport(null);
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

  const run = async (action: "start" | "stop" | "restart") => {
    const ids = Array.from(picked);
    if (ids.length === 0) return;
    setBusy(action);
    setReport(null);
    try {
      const r =
        action === "start"
          ? await api.bulkStart(ids)
          : action === "stop"
            ? await api.bulkStop(ids)
            : await api.bulkRestart(ids);
      setReport(r);
      invalidate("services");
      // 结果摘要 toast：全成功就报数量，有失败就点名
      if (r.failed.length === 0) {
        toast.success(
          t("bulk.done")
            .replace("{n}", String(r.succeeded.length))
            .replace("{action}", t(`bulk.${action}`))
        );
      } else {
        toast.warning(
          t("bulk.partial")
            .replace("{ok}", String(r.succeeded.length))
            .replace("{fail}", String(r.failed.length)),
          {
            description: r.failed
              .slice(0, 3)
              .map((f) => `${f.serviceId}: ${f.error.message}`)
              .join("\n"),
            duration: 10000,
          }
        );
      }
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  const runReportLabel = (id: string) => services.find((s) => s.id === id)?.label ?? id;

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
      >
        <ListChecks className="h-3.5 w-3.5" />
        <span className="hidden sm:inline">{t("bulk.title")}</span>
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="flex max-h-[85vh] max-w-2xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 border-b border-border px-5 py-4">
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

          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
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
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate text-[12.5px] font-medium">{s.label}</span>
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

            {/* 执行结果：按依赖顺序列出，失败项带上原始错误 */}
            {report && (
              <div className="mt-4 rounded-xl border border-border/70 bg-card-2/30 p-3">
                <div className="mb-2 flex items-center gap-2">
                  <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                    {t("bulk.execOrder")}
                  </span>
                  <span className="h-px flex-1 bg-border/60" />
                </div>
                <ol className="space-y-0.5">
                  {report.order.map((id, i) => {
                    const ok = report.succeeded.includes(id);
                    const skipped = report.already.includes(id);
                    const fail = report.failed.find((f) => f.serviceId === id);
                    return (
                      <li key={id} className="flex items-start gap-2 text-[11.5px]">
                        <span className="w-4 shrink-0 tabular text-faint">{i + 1}.</span>
                        <span className="font-mono">{runReportLabel(id)}</span>
                        <span
                          className={cn(
                            "ml-auto shrink-0",
                            ok ? "text-running" : skipped ? "text-faint" : "text-error"
                          )}
                        >
                          {ok
                            ? t("bulk.rOk")
                            : skipped
                              ? t("bulk.rSkipped")
                              : t("bulk.rFail")}
                        </span>
                        {fail && (
                          <span className="w-full pl-6 text-[10.5px] text-error">
                            {fail.error.message}
                            {fail.error.hint && (
                              <span className="text-muted"> — {fail.error.hint}</span>
                            )}
                          </span>
                        )}
                      </li>
                    );
                  })}
                </ol>
              </div>
            )}
          </div>

          <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-t border-border px-5 py-3">
            <div className="flex flex-wrap items-center gap-1.5">
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
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 text-[11.5px]"
                  onClick={() => setPicked(new Set())}
                >
                  <X className="h-3 w-3" />
                  <span className="ml-1">{t("bulk.clear")}</span>
                </Button>
              )}
            </div>

            <div className="flex items-center gap-1.5">
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
