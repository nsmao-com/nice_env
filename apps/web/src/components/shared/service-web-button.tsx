"use client";

import * as React from "react";
import { ExternalLink, Loader2 } from "lucide-react";
import type { ServiceStatus } from "@nsb/schema";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useT } from "@/lib/store";
import { toastError, useInvalidate } from "@/lib/hooks";
import { sameVersion } from "@/lib/utils";
import { normalizeError } from "@/lib/backend";
import { useInstallTasks } from "@/lib/install-tasks";
import { ConfirmDialog } from "./misc";
import * as api from "@/lib/api";

const CONSOLE_SERVICES = new Set(["sftpgo", "mailpit", "minio", "rustfs", "zincsearch", "consul", "rnacos", "qdrant", "temporal-cli", "neo4j", "rabbitmq"]);

/** 总览卡片、列表及套件页共用；网址由后端按当前运行进程确认。 */
export function ServiceWebButton({ service, disabled = false }: { service: ServiceStatus; disabled?: boolean }) {
  const t = useT();
  const [busy, setBusy] = React.useState(false);
  const [repairVersion, setRepairVersion] = React.useState<string | null>(null);
  const [repairing, setRepairing] = React.useState(false);
  const [repairError, setRepairError] = React.useState<string | null>(null);
  const [cancelling, setCancelling] = React.useState(false);
  const invalidate = useInvalidate();
  const button = React.useRef<HTMLButtonElement>(null);
  const taskId = repairVersion ? `${service.id}@${repairVersion}` : null;
  const progress = useInstallTasks((s) => taskId ? s.progress[taskId] : undefined);
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
      if (mounted.current) {
        if (normalizeError(error).code === "QDRANT_WEB_MISSING" && service.id === "qdrant" && service.version) {
          setRepairError(null); setRepairVersion(service.version);
        } else { toastError(error); }
      }
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  };
  const repair = async () => {
    if (pending.current || !repairVersion || disabled || !running || !sameVersion(service.version, repairVersion)) return;
    pending.current = true; setRepairing(true); setRepairError(null); setCancelling(false);
    useInstallTasks.setState((s) => ({ progress: Object.fromEntries(Object.entries(s.progress).filter(([key]) => key !== taskId)) }));
    try {
      const url = await api.repairServiceWebUi(service.id, repairVersion);
      if (mounted.current && current.current.id === service.id) {
        setRepairVersion(null);
        try { await api.openInBrowser(url); } catch (error) { if (mounted.current) toastError(error); }
      }
    } catch (error) {
      if (mounted.current) {
        const err = normalizeError(error);
        setRepairError(`${err.message}${err.hint ? ` · ${err.hint}` : ""}`);
      }
    } finally {
      pending.current = false;
      if (mounted.current) { setRepairing(false); setCancelling(false); }
      invalidate("services", "packages");
    }
  };
  const cancelRepair = async () => {
    if (!taskId || cancelling) return;
    setCancelling(true);
    try { if (!await api.cancelDownload(taskId) && mounted.current) setCancelling(false); }
    catch (error) { if (mounted.current) { setCancelling(false); toastError(error); } }
  };
  return <><Tooltip>
    <TooltipTrigger asChild>
      <span className="inline-flex shrink-0" tabIndex={!running ? 0 : undefined} aria-label={!running ? `${label} · ${hint}` : undefined}>
        <Button ref={button} variant="ghost" size="sm" className="min-h-8 gap-1 px-1.5 text-[11px] text-primary"
          aria-label={label} aria-busy={busy || repairing} disabled={disabled || busy || repairing || !running} onClick={() => void open()}>
          {busy || repairing ? <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" /> : <ExternalLink className="h-3.5 w-3.5" />}
          {t("svc.web.label")}
        </Button>
      </span>
    </TooltipTrigger>
    <TooltipContent>{hint}</TooltipContent>
  </Tooltip>
    <ConfirmDialog open={repairVersion !== null} onOpenChange={(value) => { if (!value && !repairing) setRepairVersion(null); }}
      title={t("svc.web.repairTitle")} description={t("svc.web.repairHint")} confirmText={t("svc.web.repairAction")}
      loading={repairing} confirmDisabled={disabled || !running || !sameVersion(service.version, repairVersion)}
      onCloseAutoFocus={(event) => { event.preventDefault(); button.current?.focus(); }} onConfirm={() => void repair()}>
      {repairing && <div className="space-y-3 text-xs text-secondary" role="status" aria-live="polite">
        <p>{cancelling ? t("install.cancelling") : progress?.state === "installed" ? t("svc.web.restarting") : t("svc.web.repairing")}</p>
        {progress && progress.total > 0 && progress.state === "downloading" && <p>{Math.min(100, Math.round(progress.received / progress.total * 100))}%</p>}
        {progress?.state !== "installed" && <Button variant="outline" size="sm" disabled={cancelling} onClick={() => void cancelRepair()}>{t("install.cancel")}</Button>}
      </div>}
      {repairError && <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{repairError}</p>}
      {!repairing && (!running || !sameVersion(service.version, repairVersion)) && <p className="text-xs text-secondary">{t("svc.web.startFirst")}</p>}
    </ConfirmDialog>
  </>;
}
