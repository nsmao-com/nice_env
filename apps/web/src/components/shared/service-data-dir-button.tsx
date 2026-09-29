"use client";

import * as React from "react";
import { FolderOpen, Loader2 } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { Button } from "@/components/ui/button";
import { useT } from "@/lib/store";
import { isTauri, normalizeError } from "@/lib/backend";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";

/** 通用服务的数据目录由 manifest 推导；内置数据库使用各自的管理页。 */
const BUILTIN_SERVICES = new Set(["nginx", "apache", "php", "mysql", "postgresql", "mongodb", "redis", "mihomo"]);

export function ServiceDataDirButton({ service, disabled = false }: { service: ServiceStatus; disabled?: boolean }) {
  const t = useT();
  const [busy, setBusy] = React.useState(false);
  const baseId = service.id.split("@")[0];
  if (!isTauri || BUILTIN_SERVICES.has(baseId) || baseId === "site-app" || baseId.startsWith("site-app:")) return null;

  const open = async () => {
    if (busy || disabled) return;
    setBusy(true);
    try {
      const path = await api.serviceDataDir(service.id);
      await api.openInFolder(path);
    } catch (error) {
      toastError(normalizeError(error));
    } finally {
      setBusy(false);
    }
  };

  const label = t("svc.dataDir" as never);
  return <Button
    type="button"
    variant="ghost"
    size="sm"
    className="min-h-8 shrink-0 gap-1.5 px-2 text-[11px] text-primary"
    disabled={disabled || busy}
    aria-label={`${label} · ${service.label}`}
    title={`${label} · ${service.label}`}
    onClick={() => void open()}
  >
    {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <FolderOpen className="h-3.5 w-3.5" />}
    {label}
  </Button>;
}
