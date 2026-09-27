"use client";

import * as React from "react";
import { ExternalLink, Loader2 } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";

const CONSOLE_SERVICES = new Set(["sftpgo", "mailpit", "minio", "consul", "rnacos", "qdrant"]);

/** 总览卡片、列表及套件页共用；网址由后端按当前运行进程确认。 */
export function ServiceWebButton({ service, disabled = false }: { service: ServiceStatus; disabled?: boolean }) {
  const t = useT();
  const [busy, setBusy] = React.useState(false);
  const pending = React.useRef(false);
  const mounted = React.useRef(true);
  const current = React.useRef(service);
  current.current = service;
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  if (!CONSOLE_SERVICES.has(service.id.split("@")[0])) return null;
  const running = service.state === "running";
  const label = t("svc.web.title").replace("{name}", service.label);
  const hint = !running ? t("svc.web.startFirst") : busy ? t("svc.web.checking") : label;
  const open = async () => {
    if (pending.current || disabled || !running) return;
    pending.current = true;
    setBusy(true);
    try {
      const url = await api.serviceWebUrl(service.id);
      if (!mounted.current) return;
      if (current.current.id !== service.id || current.current.state !== "running") {
        throw { code: "SERVICE_NOT_RUNNING", message: t("svc.web.startFirst") };
      }
      await api.openInBrowser(url);
    } catch (error) {
      if (mounted.current) toastError(error);
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  };
  return <Tooltip>
    <TooltipTrigger asChild>
      <span className="inline-flex shrink-0" tabIndex={!running ? 0 : undefined} aria-label={!running ? `${label} · ${hint}` : undefined}>
        <Button variant="ghost" size="sm" className="min-h-8 gap-1 px-1.5 text-[11px] text-primary"
          aria-label={label} aria-busy={busy} disabled={disabled || busy || !running} onClick={() => void open()}>
          {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" /> : <ExternalLink className="h-3.5 w-3.5" />}
          {t("svc.web.label")}
        </Button>
      </span>
    </TooltipTrigger>
    <TooltipContent>{hint}</TooltipContent>
  </Tooltip>;
}
