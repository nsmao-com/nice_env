"use client";

import * as React from "react";
import { Loader2, Rocket } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { useT } from "@/lib/store";
import { normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";

/** 独立服务的登录后自动启动偏好；与当前运行开关分离，避免误触发服务启停。 */
export function ServiceAutoStartButton({ service, disabled = false }: { service: ServiceStatus; disabled?: boolean }) {
  const t = useT();
  const queryClient = useQueryClient();
  const [busy, setBusy] = React.useState(false);
  // 站点应用的 runtime 类别也允许设置；没有类别的临时/未知服务不显示入口。
  if (!service.category) return null;

  const label = service.autoStart ? t("svc.autoStartDisable") : t("svc.autoStartEnable");
  const toggle = async () => {
    if (busy || disabled) return;
    setBusy(true);
    const next = !service.autoStart;
    try {
      await api.setServiceAutoStart(service.id, next);
      queryClient.setQueryData<ServiceStatus[]>(["services"], (previous) =>
        previous?.map((item) => item.id === service.id ? { ...item, autoStart: next } : item)
      );
      toast.success(t(next ? "svc.autoStartEnabled" : "svc.autoStartDisabled"));
    } catch (error) {
      toast.error(normalizeError(error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-sm"
      className={service.autoStart ? "text-primary" : "text-faint hover:text-secondary"}
      aria-label={`${label} · ${service.label}`}
      aria-pressed={service.autoStart}
      title={label}
      disabled={disabled || busy}
      onClick={() => void toggle()}
    >
      {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Rocket className="h-3.5 w-3.5" />}
    </Button>
  );
}
