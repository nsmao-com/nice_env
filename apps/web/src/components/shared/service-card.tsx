"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
import { motion } from "motion/react";
import { Cpu, RotateCw, ScrollText, Server, ShieldAlert, AlertTriangle, Stethoscope } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { ServiceSwitch } from "./service-switch";
import { StatusLight } from "./status-light";
import { StatChip } from "./stat-chip";
import { ServiceDiagnostics } from "./service-diagnostics";
import { ServiceIcon } from "./service-icon";
import { useUI, useT } from "@/lib/store";
import { useInvalidate, toastError, toastPortConflict } from "@/lib/hooks";
import { fmtUptime } from "@/lib/utils";
import * as api from "@/lib/api";

/** 服务卡片：运行=极弱呼吸光，错误=脉冲红；开关即启停 */
export function ServiceCard({ service }: { service: ServiceStatus }) {
  const t = useT();
  const router = useRouter();
  const invalidate = useInvalidate();
  const [busy, setBusy] = React.useState(false);
  const [diagOpen, setDiagOpen] = React.useState(false);
  const running = service.state === "running";
  const error = service.state === "error";
  /** 端口冲突时后端会带上端口与占用 pid：卡片上直接给「结束占用并重试」 */
  const conflict =
    service.lastError?.code === "PORT_IN_USE" && service.lastError.port != null
      ? { port: service.lastError.port, holder: service.lastError.holder }
      : null;

  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      if (next) await api.startService(service.id);
      else await api.stopService(service.id);
    } catch (e) {
      // 端口冲突：给带动作按钮的提示，用户点一下就能收掉占用者
      if (!toastPortConflict(e, { onResolved: () => invalidate("services") })) toastError(e);
    } finally {
      setBusy(false);
      invalidate("services");
    }
  };

  /** 改完配置/出错后的高频动作：就地重启，不用先停再启两步 */
  const restart = async () => {
    setBusy(true);
    try {
      await api.restartService(service.id);
      toast.success(`${service.label} · ${t("common.running")}`);
    } catch (e) {
      if (!toastPortConflict(e, { onResolved: () => invalidate("services") })) toastError(e);
    } finally {
      setBusy(false);
      invalidate("services");
    }
  };

  const stateLabel: Record<string, string> = {
    running: t("state.running"),
    stopped: t("state.stopped"),
    error: t("state.error"),
    starting: t("state.starting"),
    stopping: t("state.stopping"),
    unknown: t("state.unknown"),
  };

  // 不用 layout 动画：服务状态每 2s 轮询一次，每张卡片每次都会重渲染，
  // layout 会在每次渲染时测量 DOM（强制回流），卡片一多就明显掉帧
  return (
    <motion.div initial={{ opacity: 0, scale: 0.98 }} animate={{ opacity: 1, scale: 1 }} exit={{ opacity: 0, scale: 0.98 }}>
      <Card
        className={cn(
          "group flex flex-col gap-3 p-4 transition-[box-shadow,border-color,transform] duration-200 hover:-translate-y-0.5 hover:border-border-strong hover:shadow-[var(--shadow-raised)]",
          running && "breath border-running/25",
          error && "pulse-error border-error/30"
        )}
      >
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2.5">
            <div
              className={cn(
                "flex h-9 w-9 shrink-0 items-center justify-center rounded-[10px] border transition-colors",
                running
                  ? "border-running/25 bg-running-soft shadow-[0_0_0_3px_hsl(145_60%_45%/0.07)]"
                  : error
                    ? "border-error/25 bg-error-soft shadow-[0_0_0_3px_hsl(0_72%_50%/0.06)]"
                    : "border-border bg-card-2/70"
              )}
            >
              <ServiceIcon id={service.id} className="h-[18px] w-[18px]" />
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <span className="truncate text-[13.5px] font-semibold tracking-tight">{service.label}</span>
                <StatusLight state={service.state} size={6} />
              </div>
              <span className="text-[11px] text-faint">
                {stateLabel[service.state]}
                {service.version ? ` · ${service.version}` : ""}
              </span>
            </div>
          </div>
          <ServiceSwitch
            checked={running}
            busy={busy || service.state === "starting" || service.state === "stopping"}
            disabled={service.state === "starting" || service.state === "stopping"}
            onCheckedChange={toggle}
          />
        </div>

        {/* 前置依赖缺失：在启动前就提示，而不是让用户点了开关再看报错
            （Tomcat 缺 JDK、Composer 缺 PHP 都属于这类） */}
        {service.missingRequires.length > 0 && (
          <div className="flex items-start gap-2 rounded-lg border border-warn/25 bg-warn-soft px-2.5 py-1.5">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
            <span className="text-[11px] leading-relaxed">
              {t("svc.needDeps")}
              <span className="font-mono">{service.missingRequires.join(", ")}</span>
            </span>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-1.5">
          {service.port != null && (
            <StatChip icon={Server} title={t("svc.port")}>
              :{service.port}
            </StatChip>
          )}
          {service.memoryMb != null && (
            <StatChip icon={Cpu} title={t("svc.memory")}>
              {service.memoryMb.toFixed(0)} MB
            </StatChip>
          )}
          {running && service.uptimeSec != null && (
            <StatChip icon={RotateCw} title={t("svc.uptime")}>
              {fmtUptime(service.uptimeSec)}
            </StatChip>
          )}
          {service.logFile ? (
            <>
              <Button
                variant="ghost"
                size="icon-sm"
                className="ml-auto text-faint hover:text-secondary"
                title={t("svc.diagnose")}
                onClick={() => setDiagOpen(true)}
              >
                <Stethoscope className="h-3.5 w-3.5" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-faint hover:text-secondary"
                title={t("common.restart")}
                disabled={busy || service.state === "starting" || service.state === "stopping"}
                onClick={restart}
              >
                <RotateCw className="h-3.5 w-3.5" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-faint hover:text-secondary"
                title={t("logs.title")}
                onClick={() => router.push(`/logs?service=${encodeURIComponent(service.id)}`)}
              >
                <ScrollText className="h-3.5 w-3.5" />
              </Button>
            </>
          ) : (
            <Button
              variant="ghost"
              size="icon-sm"
              className="ml-auto text-faint hover:text-secondary"
              title={t("svc.diagnose")}
              onClick={() => setDiagOpen(true)}
            >
              <Stethoscope className="h-3.5 w-3.5" />
            </Button>
          )}
        </div>

        {error && service.lastError && (
          <div className="rounded-lg border border-error/25 bg-error-soft px-3 py-2 text-[11.5px] text-error">
            {service.lastError.message}
            {service.lastError.hint && (
              <span className="mt-0.5 block text-error/70">{service.lastError.hint}</span>
            )}
            {conflict && (
              <Button
                size="sm"
                variant="secondary"
                className="mt-2 h-7 gap-1.5 border-error/30 text-error hover:text-error"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  try {
                    await api.closePort(conflict.port);
                    toast.success(`${t("tools.portFreed")} :${conflict.port}`);
                    // 端点已经释放，直接把服务拉起来
                    await api.startService(service.id);
                    toast.success(t("common.running"));
                  } catch (e) {
                    toastError(e);
                  } finally {
                    setBusy(false);
                    invalidate("services");
                  }
                }}
              >
                <ShieldAlert className="h-3.5 w-3.5" />
                {t("svc.freePortAndRetry")} :{conflict.port}
              </Button>
            )}
          </div>
        )}
      </Card>
      <ServiceDiagnostics service={service} open={diagOpen} onOpenChange={setDiagOpen} />
    </motion.div>
  );
}
