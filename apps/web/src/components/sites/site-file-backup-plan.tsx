"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { isTauri, listen, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { BackupScheduleFields } from "@/components/shared/database-backup-plan";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";

export function SiteFileBackupPlan({ siteId, revision, active, disabled, onBusyChange }: {
  siteId: string; revision: number; active: boolean; disabled: boolean; onBusyChange: (busy: boolean) => void;
}) {
  const t = useT();
  const id = React.useId();
  const client = useQueryClient();
  const key = ["site-file-plan", siteId];
  const plan = useQuery({ queryKey: key, queryFn: () => api.siteFilesPlan(siteId), enabled: active, retry: false, refetchInterval: active ? 10000 : false });
  const [draft, setDraft] = React.useState<api.SiteFilePlan | null>(null);
  const [step, setStep] = React.useState<1 | 2>(1);
  const [confirmed, setConfirmed] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const lock = React.useRef(false);
  const opener = React.useRef<HTMLButtonElement | null>(null);
  const [error, setError] = React.useState("");
  const errorRef = React.useRef<HTMLParagraphElement | null>(null);
  const [progress, setProgress] = React.useState<api.SiteFileProgress | null>(null);
  const scope = useQuery({ queryKey: ["site-plan-scope", siteId, revision, draft?.project, draft?.excludeGenerated],
    queryFn: () => api.siteFilesScope(siteId, draft!.project, draft!.excludeGenerated),
    enabled: !!draft?.status.config.enabled && step === 2 && !busy && !disabled, retry: false, staleTime: 0 });
  const running = plan.data?.status.state === "running";
  React.useEffect(() => { onBusyChange(busy || !!draft || running); return () => onBusyChange(false); }, [busy, draft, running, onBusyChange]);
  React.useEffect(() => { setConfirmed(false); }, [revision, draft?.project, draft?.excludeGenerated, scope.data?.revision]);
  React.useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);
  React.useEffect(() => {
    if (plan.data?.status.finishedAt) void client.invalidateQueries({ queryKey: ["site-files", siteId] });
  }, [plan.data?.status.finishedAt, client, siteId]);
  React.useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ siteId: string; state: string; message: string }>("site-backup://status", (event) => {
      if (event.siteId && event.siteId !== siteId) return;
      void client.invalidateQueries({ queryKey: ["site-file-plan", siteId] });
      void client.invalidateQueries({ queryKey: ["site-files", siteId] });
      if (!event.siteId) setError(event.message);
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch((failure) => { if (!disposed) setError(normalizeError(failure).message); });
    return () => { disposed = true; unlisten?.(); };
  }, [client, siteId]);
  const begin = (value: api.SiteFilePlan) => { setDraft(structuredClone(value)); setStep(1); setConfirmed(false); setError(""); };
  const valid = !!draft && /^([01]\d|2[0-3]):[0-5]\d$/.test(draft.status.config.time)
    && Number.isInteger(draft.status.config.keep) && draft.status.config.keep >= 0 && draft.status.config.keep <= 100;
  const stale = !!draft && !!plan.data && draft.revision !== plan.data.revision;
  const run = async (action: (operationId: string) => Promise<api.SiteFilePlan>, saving: boolean) => {
    if (lock.current || disabled || running) return;
    lock.current = true; setBusy(true); setError(""); setProgress(null);
    let unlisten: (() => void) | undefined;
    try {
      const operationId = crypto.randomUUID();
      unlisten = await listen<api.SiteFileProgress>("site-files://progress", (event) => {
        if (event.siteId === siteId && event.operationId === operationId) setProgress(event);
      });
      const result = await action(operationId);
      await client.cancelQueries({ queryKey: key });
      client.setQueryData(key, result);
      if (saving) { setDraft(null); toast.success(t("settings.saved")); }
      else if (result.status.state === "success") toast.success(t("siteFiles.created"));
      else setError(result.status.message || t("pgSchedule.failed"));
    } catch (failure) {
      const parsed = normalizeError(failure);
      setError([parsed.message, parsed.hint].filter(Boolean).join(" · "));
      if (parsed.code === "SITE_BACKUP_CHANGED") { setConfirmed(false); void scope.refetch(); }
    } finally {
      unlisten?.(); lock.current = false; setBusy(false); setProgress(null);
      void client.invalidateQueries({ queryKey: key });
      void client.invalidateQueries({ queryKey: ["site-files", siteId] });
    }
  };
  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!draft || stale || !valid || busy || disabled) return;
    if (draft.status.config.enabled && step === 1) { setStep(2); return; }
    if (draft.status.config.enabled && (!confirmed || !scope.data || scope.isFetching || scope.isError)) return;
    void run(() => api.siteFilesPlanSave(siteId, draft.status.config, draft.project, draft.excludeGenerated, draft.revision, scope.data?.revision ?? null, confirmed), true);
  };
  const statusKeys = { idle: "pgSchedule.idle", running: "pgSchedule.running", success: "pgSchedule.success", failed: "pgSchedule.failed", partial: "pgSchedule.partial", interrupted: "pgSchedule.interrupted", "needs-review": "siteSchedule.review" } as const;
  const status = busy && !draft ? "running" : plan.data?.status.state || "idle";
  const failure = /failed|partial|interrupted|needs-review/.test(status);
  const errorView = error && <p ref={errorRef} tabIndex={-1} role="alert" className="rounded-lg bg-error-soft p-3 text-xs leading-relaxed text-error [overflow-wrap:anywhere]">{error}</p>;
  const progressView = busy && <p role="status" className="text-xs text-muted">{t("siteFiles.working")}{progress && ` · ${t("siteFiles.files").replace("{count}", String(progress.files))} · ${fmtBytes(progress.bytes)}`}</p>;
  return <>
    <section className="min-w-0 space-y-3 rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h3 className="flex flex-wrap items-center gap-2 text-sm font-semibold"><CalendarClock className="size-4 shrink-0" />{t("siteSchedule.title")}<span className="text-xs font-normal text-muted">{plan.data ? t(plan.data.status.config.enabled ? "pgSchedule.on" : "pgSchedule.off") : ""}</span></h3>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="ghost" disabled={disabled || busy || running || !plan.data} onClick={(event) => { opener.current = event.currentTarget; begin(plan.data!); }}>{t("pgSchedule.configure")}</Button>
          <Button size="sm" variant="secondary" disabled={disabled || busy || running || !plan.data?.scope || plan.data.status.state === "needs-review"} onClick={() => void run((operationId) => api.siteFilesPlanRun(siteId, operationId), false)}>{t("pgSchedule.run")}</Button>
        </div>
      </div>
      <p className="text-xs leading-relaxed text-muted">{t("siteSchedule.hint")}</p>
      {!isTauri && <p className="text-xs leading-relaxed text-warn">{t("siteSchedule.preview")}</p>}
      {plan.isPending ? <p role="status" className="text-xs text-muted">{t("common.loading")}</p>
        : plan.isError ? <div role="alert" className="space-y-2 text-xs text-error [overflow-wrap:anywhere]"><p>{normalizeError(plan.error).message}</p><Button size="sm" variant="ghost" onClick={() => void plan.refetch()}>{t("siteFiles.retry")}</Button></div>
        : plan.data && <div className="space-y-2 border-t border-dashed border-separator pt-3 text-xs text-muted">
          {plan.data.scope && <p className="font-mono [overflow-wrap:anywhere]">{plan.data.scope.root}</p>}
          <div className="flex flex-wrap gap-x-5 gap-y-2"><p>{t("pgSchedule.next")}: {plan.data.status.nextAt ? new Date(plan.data.status.nextAt).toLocaleString() : "—"}</p><p>{t("pgSchedule.keep")}: {plan.data.status.config.keep || t("pgSchedule.keepAll")}</p></div>
          <p>{t("pgSchedule.last")}: {plan.data.status.lastRunAt ? new Date(plan.data.status.lastRunAt).toLocaleString() : "—"} · {t(statusKeys[status as keyof typeof statusKeys] ?? "pgSchedule.failed")}</p>
          {!!plan.data.status.message && <p role={failure ? "alert" : "status"} className={`${failure ? "text-error" : ""} [overflow-wrap:anywhere]`}>{plan.data.status.message}</p>}
          {!!plan.data.status.files.length && <details><summary className="cursor-pointer">{t("pgSchedule.files")}</summary><ul className="mt-2 space-y-1">{plan.data.status.files.map((file) => <li key={file} className="font-mono [overflow-wrap:anywhere]">{file}</li>)}</ul></details>}
        </div>}
      {!draft && <>{progressView}{errorView}</>}
    </section>
    <Dialog open={!!draft} onOpenChange={(open) => { if (!open && !lock.current) { setDraft(null); setError(""); } }}>
      <DialogContent hideClose={busy} className="flex max-h-[85dvh] max-w-xl flex-col overflow-hidden" onCloseAutoFocus={(event) => { if (opener.current?.isConnected) { event.preventDefault(); opener.current.focus(); } }}>
        <DialogHeader><DialogTitle className="pr-5 leading-snug">{t("siteSchedule.title")}</DialogTitle><DialogDescription>{t(step === 1 ? "siteSchedule.scheduleStep" : "siteSchedule.scopeStep")}</DialogDescription></DialogHeader>
        {draft && <form onSubmit={submit} className="flex min-h-0 flex-col gap-4">
          <div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
            {step === 1 ? <>
              <div className="flex items-center justify-between gap-3"><Label htmlFor={`${id}-enabled`}>{t("pgSchedule.enable")}</Label><Switch id={`${id}-enabled`} checked={draft.status.config.enabled} disabled={busy} onCheckedChange={(enabled) => { setConfirmed(false); setDraft({ ...draft, status: { ...draft.status, config: { ...draft.status.config, enabled } } }); }} /></div>
              {draft.status.config.enabled ? <BackupScheduleFields value={draft.status.config} disabled={busy} keepHint={t("siteSchedule.keepHint")} onChange={(config) => { setConfirmed(false); setDraft({ ...draft, status: { ...draft.status, config } }); }} /> : <p className="text-xs leading-relaxed text-muted">{t("siteSchedule.disabledHint")}</p>}
            </> : <>
              <div className="space-y-2"><Label htmlFor={`${id}-source`}>{t("siteFiles.source")}</Label><Select value={draft.project ? "project" : "web"} disabled={busy} onValueChange={(value) => { setConfirmed(false); setDraft({ ...draft, project: value === "project" }); }}><SelectTrigger id={`${id}-source`}><SelectValue /></SelectTrigger><SelectContent><SelectItem value="project">{t("siteFiles.project")}</SelectItem><SelectItem value="web">{t("siteFiles.web")}</SelectItem></SelectContent></Select></div>
              <div className="flex items-center justify-between gap-3"><Label htmlFor={`${id}-exclude`}>{t("siteFiles.exclude")}</Label><Switch id={`${id}-exclude`} disabled={busy} checked={draft.excludeGenerated} onCheckedChange={(excludeGenerated) => { setConfirmed(false); setDraft({ ...draft, excludeGenerated }); }} /></div>
              {scope.isFetching ? <p role="status" className="text-xs text-muted">{t("common.loading")}</p> : scope.isError ? <div role="alert" className="text-xs text-error [overflow-wrap:anywhere]"><p>{normalizeError(scope.error).message}</p><Button type="button" size="sm" variant="ghost" disabled={busy} onClick={() => { setConfirmed(false); void scope.refetch(); }}>{t("siteFiles.retry")}</Button></div> : scope.data && <div className="space-y-2 rounded-lg bg-fill p-3 text-xs [overflow-wrap:anywhere]"><p className="font-mono">{scope.data.root}</p><p className="text-muted">{t("siteFiles.excluded")}: {scope.data.excluded.join(", ") || t("siteFiles.noExclusions")}</p></div>}
              <div className="space-y-2 border-t border-dashed border-separator pt-4 text-xs leading-relaxed text-muted"><p>{t("siteSchedule.hint")}</p><p>{t("siteFiles.includes")}</p><p>{t("siteSchedule.keepHint")}</p><p>{t("siteFiles.limits")}</p></div>
              <label className="flex items-start gap-2.5 text-xs leading-relaxed"><input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={confirmed} disabled={busy || scope.isFetching || scope.isError || !scope.data} onChange={(event) => setConfirmed(event.target.checked)} /><span>{t("siteSchedule.confirm").replace("{keep}", draft.status.config.keep ? String(draft.status.config.keep) : t("pgSchedule.keepAll"))}</span></label>
            </>}
            {stale && <div role="alert" className="space-y-2 text-xs text-warn"><p>{t("siteSchedule.stale")}</p><Button type="button" size="sm" variant="secondary" disabled={busy} onClick={() => begin(plan.data!)}>{t("siteSchedule.reload")}</Button></div>}
            {progressView}{errorView}
          </div>
          <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-4">
            <Button type="button" variant="ghost" disabled={busy} onClick={() => { setDraft(null); setError(""); }}>{t("common.cancel")}</Button>
            {step === 2 && <Button type="button" variant="secondary" disabled={busy} onClick={() => { setStep(1); setConfirmed(false); }}>{t("siteFiles.previous")}</Button>}
            <Button type="submit" disabled={disabled || busy || running || stale || !valid || (step === 2 && (!confirmed || !scope.data || scope.isFetching || scope.isError))}>{busy ? t("confirm.busy") : step === 1 && draft.status.config.enabled ? t("siteFiles.next") : t("common.save")}</Button>
          </DialogFooter>
        </form>}
      </DialogContent>
    </Dialog>
  </>;
}
