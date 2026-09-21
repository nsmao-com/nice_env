"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
import { motion } from "motion/react";
import { RotateCw, ScrollText, Server, ShieldAlert } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { cn, fmtUptime } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ServiceSwitch } from "./service-switch";
import { StatusLight } from "./status-light";
import { useT } from "@/lib/store";
import { useInvalidate, toastError, toastPortConflict } from "@/lib/hooks";
import * as api from "@/lib/api";

/**
 * 服务列表行：一屏能看更多服务。
 * 与 ServiceCard 共用同一套启停/端口冲突处理逻辑，只是把信息压成一行。
 */
export function ServiceRow({ service }: { service: ServiceStatus }) {
  const t = useT();
  const router = useRouter();
  const invalidate = useInvalidate();
  const [busy, setBusy] = React.useState(false);
  const running = service.state === "running";
  const error = service.state === "error";
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
      if (!toastPortConflict(e, { onResolved: () => invalidate("services") })) toastError(e);
    } finally {
      setBusy(false);
      invalidate("services");
    }
  };

  /** 就地重启：改配置 / 出错后的高频动作，省去先停再启两步 */
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

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 4 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0 }}
      className={cn(
        "group/item flex items-center gap-3 rounded-lg border px-3 py-2 transition-colors",
        error
          ? "border-error/30 bg-error-soft/40"
          : running
            ? "border-border bg-card hover:bg-card-2/40"
            : "border-border/60 bg-transparent hover:bg-card-2/40"
      )}
    >
      <StatusLight state={service.state} size={7} />

      <span className="min-w-0 flex-1 truncate text-[13px] font-medium">{service.label}</span>

      {service.version && (
        <span className="hidden shrink-0 font-mono text-[11px] text-faint sm:inline">
          {service.version}
        </span>
      )}

      {service.port != null && (
        <span className="hidden shrink-0 items-center gap-1 font-mono text-[11px] text-muted sm:flex">
          <Server className="h-3 w-3 text-faint" strokeWidth={1.8} />:{service.port}
        </span>
      )}

      {service.memoryMb != null && running && (
        <span className="hidden shrink-0 font-mono text-[11px] text-muted md:inline">
          {service.memoryMb.toFixed(0)} MB
        </span>
      )}

      {running && service.uptimeSec != null && (
        <span className="hidden shrink-0 items-center gap-1 text-[11px] text-faint md:flex">
          <RotateCw className="h-3 w-3" strokeWidth={1.8} />
          {fmtUptime(service.uptimeSec)}
        </span>
      )}

      <span className={cn("shrink-0 text-[11px]", error ? "text-error" : "text-faint")}>
        {stateLabel[service.state]}
      </span>

      {/* 端口冲突：一行内直接给「结束占用并重试」 */}
      {conflict && (
        <Button
          size="sm"
          variant="ghost"
          className="h-6 shrink-0 gap-1 px-1.5 text-[11px] text-error hover:text-error"
          disabled={busy}
          title={conflict.holder ? `被 ${conflict.holder} 占用` : undefined}
          onClick={async () => {
            setBusy(true);
            try {
              await api.closePort(conflict.port);
              await api.startService(service.id);
            } catch (e) {
              toastError(e);
            } finally {
              setBusy(false);
              invalidate("services");
            }
          }}
        >
          <ShieldAlert className="h-3 w-3" />:{conflict.port}
        </Button>
      )}

      {service.logFile && (
        <>
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0 text-faint opacity-0 transition-opacity hover:text-secondary group-hover/item:opacity-100"
            title={t("common.restart")}
            disabled={busy || service.state === "starting" || service.state === "stopping"}
            onClick={restart}
          >
            <RotateCw className="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0 text-faint opacity-0 transition-opacity hover:text-secondary group-hover/item:opacity-100"
            title={t("logs.title")}
            onClick={() => router.push(`/logs?service=${encodeURIComponent(service.id)}`)}
          >
            <ScrollText className="h-3.5 w-3.5" />
          </Button>
        </>
      )}

      <ServiceSwitch
        checked={running}
        busy={busy || service.state === "starting" || service.state === "stopping"}
        disabled={service.state === "starting" || service.state === "stopping"}
        onCheckedChange={toggle}
      />
    </motion.div>
  );
}
