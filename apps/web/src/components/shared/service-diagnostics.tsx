"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  AlertTriangle,
  CheckCircle2,
  Loader2,
  MinusCircle,
  RotateCw,
  ScrollText,
  Stethoscope,
  XCircle,
} from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { cn, fmtUptime } from "@/lib/utils";
import { useT } from "@/lib/store";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/* ============================================================
   服务诊断：把已有的后端能力（端口诊断 / 配置校验 / 日志扫描）
   编排成一张体检单，帮用户回答「这个服务为什么起不来 / 不正常」。
   纯前端编排，不新增 Rust 命令；单项失败只影响该项，不中断整体。
   ============================================================ */

type CheckState = "pending" | "running" | "ok" | "warn" | "error" | "skip";

interface CheckItem {
  id: string;
  title: string;
  state: CheckState;
  detail?: string;
  /** 日志扫描命中时展示的原文行 */
  lines?: string[];
}

/** 服务 id（php-8.3 / nginx / mysql …）→ 配置编辑器的校验 kind */
function configKindFor(serviceId: string): string | null {
  const id = serviceId.toLowerCase();
  if (id.includes("nginx")) return "nginx-main";
  if (id.includes("apache")) return "apache-conf";
  if (id.includes("php")) return "php-ini";
  if (id.includes("maria") || id.includes("mysql")) return "mysql-ini";
  if (id.includes("redis")) return "redis-conf";
  return null;
}

/** 日志里值得上报的关键词 —— 只做提示，不下结论 */
const LOG_ERROR_RE =
  /\b(error|fatal|panic|emerg|critical|exception|uncaught|traceback|failed|failure|refused|denied|segfault)\b/i;

function shortErr(e: unknown): string {
  return String(e).slice(0, 140);
}

export function ServiceDiagnostics({
  service,
  open,
  onOpenChange,
}: {
  service: ServiceStatus;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useT();
  const router = useRouter();
  const [items, setItems] = React.useState<CheckItem[]>([]);
  const [running, setRunning] = React.useState(false);

  const patch = React.useCallback((id: string, p: Partial<CheckItem>) => {
    setItems((s) => s.map((it) => (it.id === id ? { ...it, ...p } : it)));
  }, []);

  const run = React.useCallback(async () => {
    setRunning(true);

    const stateLabel: Record<string, string> = {
      running: t("state.running"),
      stopped: t("state.stopped"),
      error: t("state.error"),
      starting: t("state.starting"),
      stopping: t("state.stopping"),
      unknown: t("state.unknown"),
    };

    setItems([
      { id: "status", title: t("svc.diag.check.status"), state: "running" },
      { id: "port", title: t("svc.diag.check.port"), state: "running" },
      { id: "config", title: t("svc.diag.check.config"), state: "running" },
      { id: "logs", title: t("svc.diag.check.logs"), state: "running" },
    ]);

    /* ---- 1. 服务状态（同步来自卡片数据）---- */
    const stateDetail =
      service.state === "running"
        ? [
            stateLabel[service.state],
            service.uptimeSec != null ? fmtUptime(service.uptimeSec) : null,
            service.memoryMb != null ? `${service.memoryMb.toFixed(0)} MB` : null,
            service.version ? `v${service.version}` : null,
          ]
            .filter(Boolean)
            .join(" · ")
        : service.state === "error" && service.lastError
          ? service.lastError.message
          : stateLabel[service.state] ?? service.state;
    patch("status", {
      state: service.state === "running" ? "ok" : service.state === "error" ? "error" : "warn",
      detail: stateDetail,
    });

    const jobs: Promise<void>[] = [];

    /* ---- 2. 端口监听 ---- */
    if (service.port == null) {
      patch("port", { state: "skip", detail: t("svc.diag.noPort") });
    } else {
      jobs.push(
        api
          .diagnosePort(service.port)
          .then((d) => {
            if (!d.inUse) {
              patch(
                "port",
                service.state === "running"
                  ? { state: "warn", detail: t("svc.diag.portSilent") }
                  : { state: "ok", detail: t("svc.diag.portFree").replace("{port}", String(service.port)) }
              );
            } else if (service.state === "running") {
              patch("port", {
                state: "ok",
                detail: `${t("svc.diag.portListening")} · ${d.processName ?? "?"} (pid ${d.pid ?? "?"})`,
              });
            } else {
              patch("port", {
                state: "error",
                detail: `${t("svc.diag.portConflict")} :${service.port} · ${d.processName ?? "?"} (pid ${d.pid ?? "?"})`,
              });
            }
          })
          .catch((e) => patch("port", { state: "error", detail: shortErr(e) }))
      );
    }

    /* ---- 3. 配置文件校验（没装对应套件 → 跳过，不算失败）---- */
    const kind = configKindFor(service.id);
    if (!kind) {
      patch("config", { state: "skip", detail: t("svc.diag.noConfig") });
    } else {
      jobs.push(
        (async () => {
          try {
            const content = await api.configRead(kind);
            const v = await api.configValidate(kind, content);
            if (v.ok) {
              patch("config", { state: "ok", detail: t("svc.diag.configOk") });
            } else {
              const hasErr = v.issues.some((i) => i.severity === "error");
              patch("config", {
                state: hasErr ? "error" : "warn",
                detail:
                  t("svc.diag.configIssues").replace("{n}", String(v.issues.length)) +
                  (v.issues[0] ? ` · ${v.issues[0].message}` : ""),
              });
            }
          } catch (e) {
            patch("config", { state: "skip", detail: shortErr(e) });
          }
        })()
      );
    }

    /* ---- 4. 日志异常扫描 ---- */
    if (!service.logFile) {
      patch("logs", { state: "skip", detail: t("svc.diag.noLogFile") });
    } else {
      jobs.push(
        api
          .tailLogs(service.id, 300)
          .then((rows) => {
            const hits = rows.filter((r) => LOG_ERROR_RE.test(r.line));
            if (hits.length === 0) {
              patch("logs", { state: "ok", detail: t("svc.diag.logsClean").replace("{n}", String(rows.length)) });
            } else {
              patch("logs", {
                state: "warn",
                detail: t("svc.diag.logsIssues").replace("{n}", String(hits.length)),
                lines: hits.slice(-3).map((r) => r.line.slice(0, 200)),
              });
            }
          })
          .catch((e) => patch("logs", { state: "skip", detail: shortErr(e) }))
      );
    }

    await Promise.all(jobs);
    setRunning(false);
  }, [service, t, patch]);

  // 只在打开时跑一次；状态轮询带来的 service 引用变化不重跑
  React.useEffect(() => {
    if (open) void run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg gap-0 overflow-hidden p-0">
        <DialogHeader className="border-b border-border px-5 py-3.5">
          <DialogTitle className="flex items-center gap-2 text-[14px]">
            <Stethoscope className="h-4 w-4 text-primary" strokeWidth={1.8} />
            {t("svc.diag.title").replace("{name}", service.label)}
          </DialogTitle>
          <DialogDescription className="text-[11px]">{t("svc.diag.subtitle")}</DialogDescription>
        </DialogHeader>

        <div className="flex max-h-[55vh] flex-col gap-1.5 overflow-y-auto px-5 py-4">
          {items.map((it) => (
            <CheckRow key={it.id} item={it} />
          ))}
        </div>

        <div className="flex items-center justify-between border-t border-border px-5 py-3">
          <Button
            variant="ghost"
            size="sm"
            className="h-7 text-[11.5px] text-faint hover:text-foreground"
            disabled={!service.logFile}
            onClick={() => router.push(`/logs?service=${encodeURIComponent(service.id)}`)}
          >
            <ScrollText className="h-3.5 w-3.5" />
            {t("svc.diag.viewLogs")}
          </Button>
          <Button size="sm" variant="secondary" className="h-7" disabled={running} onClick={() => void run()}>
            <RotateCw className={cn("h-3.5 w-3.5", running && "animate-spin")} />
            {t("svc.diag.rerun")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function CheckRow({ item }: { item: CheckItem }) {
  const stateMark: Record<CheckState, React.ReactNode> = {
    pending: <MinusCircle className="h-3.5 w-3.5 shrink-0 text-faint/60" />,
    running: <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-primary" />,
    ok: <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-running" />,
    warn: <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-warn" />,
    error: <XCircle className="h-3.5 w-3.5 shrink-0 text-error" />,
    skip: <MinusCircle className="h-3.5 w-3.5 shrink-0 text-faint" />,
  };
  return (
    <div
      className={cn(
        "rounded-lg border px-2.5 py-2",
        item.state === "error" && "border-error/25 bg-error-soft",
        item.state === "warn" && "border-warn/25 bg-warn-soft",
        item.state !== "error" && item.state !== "warn" && "border-border/60"
      )}
    >
      <div className="flex items-start gap-2">
        <span className="mt-0.5">{stateMark[item.state]}</span>
        <div className="min-w-0 flex-1">
          <p className="text-[12px] font-medium">{item.title}</p>
          {item.detail && (
            <p
              className={cn(
                "mt-0.5 break-all text-[11px] leading-relaxed",
                item.state === "error" ? "text-error" : item.state === "warn" ? "text-warn" : "text-faint"
              )}
            >
              {item.detail}
            </p>
          )}
          {item.lines && item.lines.length > 0 && (
            <pre className="mt-1.5 max-h-28 overflow-auto rounded-md bg-card-2/50 p-2 font-mono text-[10.5px] leading-relaxed text-secondary">
              {item.lines.join("\n")}
            </pre>
          )}
        </div>
      </div>
    </div>
  );
}
