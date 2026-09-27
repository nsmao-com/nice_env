"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { FolderOpen, Loader2, RotateCw } from "lucide-react";
import { toast } from "sonner";
import type { ServiceStatus } from "@nsb/schema";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/misc";
import { normalizeError } from "@/lib/backend";
import { serviceHasProcess, useInvalidate } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";

/** 共用目录选择入口；只保存用户确认的目录，启动继续使用原服务开关。 */
export function SftpgoConfigButton({ service, disabled = false }: { service: ServiceStatus; disabled?: boolean }) {
  const t = useT();
  const invalidate = useInvalidate();
  const [open, setOpen] = React.useState(false);
  const [selected, setSelected] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);
  const [saveError, setSaveError] = React.useState<string | null>(null);
  const pending = React.useRef(false);
  const button = React.useRef<HTMLButtonElement>(null);
  const groupId = React.useId();
  const query = useQuery({
    queryKey: ["sftpgo-config-directories", service.version], queryFn: api.sftpgoConfigDirectories,
    enabled: open && service.id === "sftpgo", retry: false, staleTime: 0,
    refetchOnWindowFocus: false, refetchOnReconnect: false,
  });
  if (service.id !== "sftpgo") return null;
  const report = query.data;
  const chosen = selected ?? report?.current ?? "";
  const candidate = report?.directories.find((item) => item.directory === chosen);
  const active = serviceHasProcess(service) || service.state === "starting" || service.state === "stopping";
  const error = query.error ? normalizeError(query.error) : null;
  const changeOpen = (next: boolean) => {
    if (pending.current) return;
    setOpen(next);
    if (next) { setSelected(null); setSaveError(null); }
  };
  const save = async () => {
    if (pending.current || active || disabled || query.isFetching || query.isError || !candidate || candidate.issue || !report) return;
    pending.current = true; setSaving(true); setSaveError(null);
    try {
      await api.selectSftpgoConfig(candidate.directory, report.version, report.current);
      toast.success(t("svc.sftpgo.saved"));
      invalidate("services"); invalidate("sftpgo-config-directories");
      setOpen(false);
    } catch (error) {
      const detail = normalizeError(error);
      setSaveError([detail.message, detail.hint].filter(Boolean).join(" · "));
      // 失败保留选择，但重新读取目录、版本和绑定，重试不能覆盖别处的新选择。
      await query.refetch({ cancelRefetch: false });
    } finally { pending.current = false; setSaving(false); }
  };

  return <>
    <Button ref={button} type="button" variant="ghost" size="sm" className="h-auto min-h-8 gap-1.5 px-2 text-[11px] text-primary"
      disabled={disabled} aria-label={t("svc.sftpgo.title")} onClick={() => changeOpen(true)}>
      <FolderOpen className="h-3.5 w-3.5 shrink-0" />{t("svc.sftpgo.label")}
    </Button>
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogContent hideClose={saving} className="flex max-h-[calc(100dvh-2rem)] max-w-xl flex-col gap-0 overflow-hidden p-0"
        onCloseAutoFocus={(event) => { if (button.current?.isConnected) { event.preventDefault(); button.current.focus(); } }}>
        <DialogHeader className="shrink-0 px-4 py-4 pr-12 sm:px-5 sm:pr-12">
          <DialogTitle className="text-[14px] leading-relaxed">{t("svc.sftpgo.title")}</DialogTitle>
          <DialogDescription className="text-[11.5px] leading-relaxed">{t("svc.sftpgo.hint")}</DialogDescription>
        </DialogHeader>
        <div className="mx-2 shrink-0 border-t border-dashed border-border" />
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto overscroll-contain px-4 py-4 sm:px-5 [overflow-wrap:anywhere]" aria-busy={query.isFetching || saving}>
          {active && <p role="status" className="rounded-lg bg-warn-soft px-3 py-2 text-xs text-warn">{t("svc.sftpgo.stopFirst")}</p>}
          {error && <div role="alert" className="rounded-lg bg-error-soft px-3 py-2 text-xs text-error">
            <p>{error.message}</p>{error.hint && <p className="mt-1">{error.hint}</p>}
          </div>}
          {saveError && <p role="alert" className="rounded-lg bg-error-soft px-3 py-2 text-xs text-error">{saveError}</p>}
          {query.isFetching && <p role="status" className="flex items-center gap-2 text-xs text-muted"><Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />{t("svc.sftpgo.loading")}</p>}
          {!report && query.isFetching && <div aria-hidden="true" className="space-y-2"><Skeleton className="h-24 w-full" /><Skeleton className="h-24 w-full" /></div>}
          {report && <>
            {report.current && <p className="text-[11px] leading-relaxed text-muted">{t("svc.sftpgo.current")} <span className="font-mono text-secondary">{report.current}</span></p>}
            {report.directories.length === 0 ? <p className="rounded-lg bg-card-2/50 px-3 py-4 text-xs leading-relaxed text-muted">{t("svc.sftpgo.empty")}</p> : <fieldset disabled={saving || active || disabled || query.isFetching || query.isError} className="min-w-0 space-y-2">
              <legend className="mb-2 text-xs font-medium">{t("svc.sftpgo.choose")}</legend>
              {report.directories.map((item, index) => <label key={item.directory} className={cn("flex min-w-0 items-start gap-3 rounded-xl border px-3 py-3 transition-colors focus-within:ring-2 focus-within:ring-ring",
                chosen === item.directory ? "border-primary/50 bg-primary-soft" : "border-border bg-card-2/20", item.issue ? "cursor-not-allowed" : "cursor-pointer")}>
                <input type="radio" name={groupId} value={item.directory} checked={chosen === item.directory} disabled={!!item.issue}
                  aria-describedby={`${groupId}-${index}`} onChange={() => { setSelected(item.directory); setSaveError(null); }}
                  className="mt-1 h-4 w-4 shrink-0 accent-primary focus-visible:outline-2 focus-visible:outline-ring" />
                <span className="min-w-0 flex-1 space-y-1.5 text-[11px] leading-relaxed">
                  <span className="flex flex-wrap items-center gap-x-2 gap-y-1"><span className="text-xs font-semibold text-foreground">{item.label}</span>
                    {report.current === item.directory && <span className="rounded bg-fill px-1.5 text-muted">{t("svc.sftpgo.inUse")}</span>}</span>
                  <span className="block font-mono text-muted">{item.directory}</span>
                  <span id={`${groupId}-${index}`} className="block space-y-1">
                    <span className="block text-secondary">{t("svc.sftpgo.config")} {item.configFile ?? t("svc.sftpgo.missing")}</span>
                    {item.issue ? <span className="block text-error">{item.issue}</span> : <>
                      <span className="block text-secondary">{t("svc.sftpgo.ready")}</span>
                      <span className="block font-mono text-muted">{item.stateFiles.join(" · ")}</span>
                    </>}
                    {item.modifiedAt != null && <span className="block text-faint">{t("svc.sftpgo.modified")} <time dateTime={new Date(item.modifiedAt * 1000).toISOString()}>{new Date(item.modifiedAt * 1000).toLocaleString()}</time></span>}
                  </span>
                </span>
              </label>)}
            </fieldset>}
          </>}
        </div>
        <div className="mx-2 shrink-0 border-t border-dashed border-border" />
        <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 px-4 py-3 sm:px-5">
          <Button variant="ghost" size="sm" className="min-h-9" disabled={saving || query.isFetching} onClick={() => void query.refetch({ cancelRefetch: false })}>
            <RotateCw className="h-3.5 w-3.5" />{t("tools.refresh")}
          </Button>
          <div className="flex flex-wrap gap-2">
            <Button variant="ghost" size="sm" className="min-h-9" disabled={saving} onClick={() => changeOpen(false)}>{t("common.cancel")}</Button>
            <Button size="sm" className="h-auto min-h-9 whitespace-normal" disabled={saving || active || disabled || query.isFetching || query.isError || !candidate || !!candidate.issue || chosen === report?.current} onClick={() => void save()}>
              {saving && <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin motion-reduce:animate-none" />}{t(saving ? "confirm.busy" : "svc.sftpgo.use")}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  </>;
}
