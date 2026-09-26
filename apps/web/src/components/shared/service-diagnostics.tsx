"use client";

import { useQuery } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { AlertTriangle, CheckCircle2, Info, Loader2, MinusCircle, RotateCw, ScrollText, Stethoscope, Wrench, XCircle } from "lucide-react";
import type { ServiceCheck, ServiceStatus } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/misc";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";

/** 每个服务独立缓存；关闭或切换对象不会把旧请求的结果写到另一个服务。 */
export function ServiceDiagnostics({ service, open, onOpenChange }: {
  service: ServiceStatus; open: boolean; onOpenChange: (open: boolean) => void;
}) {
  const t = useT();
  const router = useRouter();
  const query = useQuery({
    queryKey: ["service-diagnostics", service.id],
    queryFn: () => api.diagnoseService(service.id),
    enabled: open, retry: false, networkMode: "always",
    staleTime: 0, refetchOnWindowFocus: false, refetchOnReconnect: false,
  });
  const report = query.data;
  const error = query.error ? normalizeError(query.error) : null;
  const running = query.isFetching;
  const navigate = (route: string) => { onOpenChange(false); router.push(route); };

  return <Dialog open={open} onOpenChange={onOpenChange}>
    <DialogContent className="flex max-h-[calc(100dvh-2rem)] max-w-xl flex-col gap-0 overflow-hidden p-0">
      <DialogHeader className="shrink-0 px-4 py-4 pr-12 sm:px-5 sm:pr-12">
        <DialogTitle className="flex min-w-0 items-start gap-2 text-[14px] leading-relaxed [overflow-wrap:anywhere]">
          <Stethoscope className="mt-0.5 h-4 w-4 shrink-0 text-primary" strokeWidth={1.8} />
          <span className="min-w-0">{t("svc.diag.title").replace("{name}", report?.service.label ?? service.label)}</span>
        </DialogTitle>
        <DialogDescription className="text-[11px] leading-relaxed">{t("svc.diag.subtitle")}</DialogDescription>
      </DialogHeader>
      <div className="mx-4 shrink-0 border-t border-dashed border-border sm:mx-5" />
      <div role="region" aria-label={t("svc.diag.results")} tabIndex={0} className="min-h-0 flex-1 space-y-3 overflow-y-auto overscroll-contain px-4 py-4 sm:px-5 [overflow-wrap:anywhere] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
        {error && <div role="alert" className="rounded-lg border border-error/25 bg-error-soft/40 px-3 py-2.5 text-xs text-error">
          <p className="font-medium">{t(report ? "svc.diag.refreshFailed" : "svc.diag.failed")}</p>
          <p className="mt-1 text-[11px] leading-relaxed">{error.message}</p>
          {error.hint && <p className="mt-1 text-[11px]">{error.hint}</p>}
        </div>}
        {running && <p role="status" className="flex items-start gap-2 text-[11px] leading-relaxed text-muted">
          <Loader2 className="mt-0.5 h-3.5 w-3.5 shrink-0 animate-spin motion-reduce:animate-none" />
          {t(report ? "svc.diag.refreshing" : "svc.diag.collecting")}
        </p>}
        {!report && running && <div className="space-y-2" aria-hidden="true">{[0, 1, 2, 3].map((id) => <Skeleton key={id} className="h-16 w-full" />)}</div>}
        {report && <>
          <div className="space-y-1 text-[11px] leading-relaxed text-muted">
            <p className="font-mono text-secondary">{report.service.version ? `${report.service.id.split("@")[0]} · v${report.service.version}` : report.service.id}{report.service.port != null ? ` · :${report.service.port}` : ""}</p>
            <p>{t("svc.diag.checkedAt")} <time dateTime={new Date(report.checkedAt * 1000).toISOString()}>{new Date(report.checkedAt * 1000).toLocaleString()}</time></p>
          </div>
          <div className="space-y-2">{report.checks.map((item) => <CheckRow key={item.id} item={item} />)}</div>
          {report.warnings.length > 0 && <div className="space-y-1 rounded-lg bg-card-2/40 px-3 py-2.5 text-[11px] leading-relaxed text-muted">
            {report.warnings.map((warning, index) => <p key={index}>{warning}</p>)}
          </div>}
        </>}
      </div>
      <div className="mx-4 shrink-0 border-t border-dashed border-border sm:mx-5" />
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 px-4 py-3 sm:px-5">
        <div className="flex flex-wrap gap-1">
          <Button variant="ghost" size="sm" className="min-h-9 h-auto whitespace-normal text-[11.5px]" disabled={!report?.service.logFile}
            onClick={() => navigate(`/logs?service=${encodeURIComponent(service.id)}`)}>
            <ScrollText className="h-3.5 w-3.5 shrink-0" />{t("svc.diag.viewLogs")}
          </Button>
          <Button variant="ghost" size="sm" className="min-h-9 h-auto whitespace-normal text-[11.5px]" onClick={() => navigate("/tools#nsb-tool-repair")}>
            <Wrench className="h-3.5 w-3.5 shrink-0" />{t("svc.diag.repair")}
          </Button>
        </div>
        <Button size="sm" variant="secondary" className="min-h-9 h-auto whitespace-normal" disabled={running} onClick={() => void query.refetch({ cancelRefetch: false })}>
          <RotateCw className={cn("h-3.5 w-3.5 shrink-0", running && "animate-spin motion-reduce:animate-none")} />{t("svc.diag.rerun")}
        </Button>
      </div>
    </DialogContent>
  </Dialog>;
}

function CheckRow({ item }: { item: ServiceCheck }) {
  const t = useT();
  const Icon = item.state === "ok" ? CheckCircle2 : item.state === "error" ? XCircle : item.state === "warning" || item.state === "unavailable" ? AlertTriangle : item.state === "skipped" ? MinusCircle : Info;
  const color = item.state === "error" ? "text-error" : item.state === "warning" || item.state === "unavailable" ? "text-warn" : item.state === "ok" ? "text-running" : "text-muted";
  return <div className={cn("min-w-0 rounded-lg border px-3 py-2.5", item.state === "error" ? "border-error/25 bg-error-soft/40" : item.state === "warning" || item.state === "unavailable" ? "border-warn/25 bg-warn-soft/40" : "border-border/60")}>
    <div className="flex items-start gap-2">
      <Icon className={cn("mt-0.5 h-3.5 w-3.5 shrink-0", color)} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-start justify-between gap-x-3 gap-y-1">
          <p className="text-[12px] font-medium">{t(`svc.diag.check.${item.id}`)}</p>
          <span className={cn("text-[11px]", color)}>{t(`svc.diag.state.${item.state}`)}</span>
        </div>
        <p className="mt-1 text-[10.5px] text-muted">{t(`svc.diag.method.${item.method}`)}</p>
        <p className="mt-1 whitespace-pre-wrap text-[11px] leading-relaxed text-secondary">{item.detail}</p>
        {item.lines.length > 0 && <pre className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap rounded-md bg-card-2/50 p-2 font-mono text-[10.5px] leading-relaxed text-secondary [overflow-wrap:anywhere]">{item.lines.join("\n")}</pre>}
      </div>
    </div>
  </div>;
}
