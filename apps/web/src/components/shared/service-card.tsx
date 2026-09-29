"use client";

import * as React from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
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
import { ServiceWebButton } from "./service-web-button";
import { ServiceConfigButton } from "./service-config-button";
import { SftpgoConfigButton } from "./sftpgo-config-button";
import { useT } from "@/lib/store";
import { useServiceActions } from "@/lib/hooks";
import { fmtUptime } from "@/lib/utils";

/** 服务卡片：运行=极弱呼吸光，错误=脉冲红；开关即启停 */
export function ServiceCard({ service, dragHandle, dragPreview = false }: { service: ServiceStatus; dragHandle?: React.ReactNode; dragPreview?: boolean }) {
  const t = useT();
  const router = useRouter();
  const { busy, failure, conflict, toggle, restart, resolveConflict, retry } = useServiceActions(service, dragPreview);
  const [diagOpen, setDiagOpen] = React.useState(false);
  const running = service.state === "running";
  const error = service.state === "error";
  const hasProcess = running || service.pids.length > 0;
  const dependencyBlocked = !hasProcess && service.missingRequires.length > 0;
  const transitioning = service.state === "starting" || service.state === "stopping";
  const actionDisabled = busy || transitioning || service.state === "unknown" || dragPreview;
  const problem = failure?.error ?? (error ? service.lastError : undefined);

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
    <motion.div initial={dragPreview ? false : { opacity: 0, scale: 0.98 }} animate={{ opacity: 1, scale: 1 }} exit={{ opacity: 0, scale: 0.98 }}>
      <Card
        className={cn(
          "group flex flex-col gap-3 p-4 transition-[box-shadow,border-color,transform] duration-200 hover:-translate-y-0.5 hover:border-border-strong hover:shadow-[var(--shadow-raised)]",
          running && "breath border-running/25",
          error && "pulse-error border-error/30"
        )}
      >
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2.5">
            {dragHandle}
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
            label={service.label}
            checked={hasProcess}
            busy={busy || transitioning}
            disabled={actionDisabled || dependencyBlocked}
            title={dependencyBlocked ? t("svc.needDepsHint") : undefined}
            onCheckedChange={toggle}
          />
        </div>

        {/* 前置依赖缺失：在启动前就提示，而不是让用户点了开关再看报错
            （Tomcat 缺 JDK、Composer 缺 PHP 都属于这类） */}
        {service.missingRequires.length > 0 && (
          <div className="flex items-start gap-2 rounded-lg border border-warn/25 bg-warn-soft px-2.5 py-1.5">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
            <span className="min-w-0 text-[11px] leading-relaxed">
              {t("svc.needDeps")}
              <span className="ml-1 inline-flex flex-wrap gap-x-1.5 gap-y-0.5 align-baseline">
                {service.missingRequires.map((dependency) => (
                  <Link
                    key={dependency}
                    href={`/packages?search=${encodeURIComponent(dependency)}`}
                    className="font-mono text-warn underline decoration-dashed underline-offset-2 hover:text-foreground"
                  >
                    {dependency}
                  </Link>
                ))}
              </span>
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
          <ServiceConfigButton service={service} disabled={busy || dragPreview} />
          <ServiceWebButton service={service} disabled={busy || dragPreview} />
          <SftpgoConfigButton service={service} disabled={busy || dragPreview} />
          <Button
            variant="ghost"
            size="icon-sm"
            className="ml-auto text-faint hover:text-secondary"
            title={t("svc.diagnose")}
            aria-label={t("svc.diagnose")}
            disabled={dragPreview}
            onClick={() => setDiagOpen(true)}
          >
            <Stethoscope className="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            className="text-faint hover:text-secondary"
            title={t("common.restart")}
            aria-label={t("common.restart")}
            disabled={actionDisabled || service.missingRequires.length > 0}
            onClick={restart}
          >
            <RotateCw className="h-3.5 w-3.5" />
          </Button>
          {service.logFile && (
            <Button
              variant="ghost"
              size="icon-sm"
              className="text-faint hover:text-secondary"
              title={t("logs.title")}
              aria-label={t("logs.title")}
              disabled={dragPreview}
              onClick={() => router.push(`/logs?service=${encodeURIComponent(service.id)}`)}
            >
              <ScrollText className="h-3.5 w-3.5" />
            </Button>
          )}
        </div>

        {problem && (
          <div role="alert" className="min-w-0 break-words rounded-lg border border-error/25 bg-error-soft px-3 py-2 text-[11.5px] text-error [overflow-wrap:anywhere]">
            {problem.message}
            {problem.hint && (
              <span className="mt-0.5 block text-error/70">{problem.hint}</span>
            )}
            {conflict && (
              <Button
                size="sm"
                variant="secondary"
                className="mt-2 h-auto min-h-7 max-w-full gap-1.5 whitespace-normal border-error/30 text-error hover:text-error"
                disabled={actionDisabled || hasProcess || service.missingRequires.length > 0}
                onClick={resolveConflict}
              >
                <ShieldAlert className="h-3.5 w-3.5" />
                {t("svc.freePortAndRetry")} :{conflict.port}
              </Button>
            )}
            {failure && !conflict && (
              <Button size="sm" variant="ghost" className="mt-2 h-auto min-h-7 max-w-full gap-1.5 whitespace-normal"
                disabled={actionDisabled || (failure.action !== "stop" && service.missingRequires.length > 0)} onClick={retry}>
                <RotateCw className="h-3.5 w-3.5 shrink-0" />
                {t("bulk.retry")} · {t(`common.${failure.action}`)}
              </Button>
            )}
          </div>
        )}
      </Card>
      <ServiceDiagnostics service={service} open={diagOpen} onOpenChange={setDiagOpen} />
    </motion.div>
  );
}
