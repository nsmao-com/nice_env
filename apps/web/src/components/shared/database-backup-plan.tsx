"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { DatabaseEngine } from "@nsb/schema";
import { CalendarClock } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useInvalidate } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

/** 同一套计划表单用于三个原生引擎；实例切换和手动操作由父卡片统一锁定。 */
export function DatabaseBackupPlan({ engine, version, targetLabel, ready, disabled, onLockChange }: {
  engine: DatabaseEngine | "postgresql"; version: string; targetLabel: string; ready: boolean; disabled: boolean; onLockChange: (locked: boolean) => void;
}) {
  const t = useT();
  const id = React.useId();
  const identity = `${engine}@${version}`;
  const queryKey = ["database-backup-plan", engine, version];
  const filesKey = engine === "postgresql" ? "postgres-backups" : "db-backups";
  const invalidate = useInvalidate();
  const queryClient = useQueryClient();
  const lock = React.useRef(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const fail = (error: unknown) => { const parsed = normalizeError(error); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); };
  const plan = useQuery({ queryKey, queryFn: () => engine === "postgresql" ? api.postgresBackupPlan(version) : api.dbBackupPlan(engine, version), enabled: !!version, retry: false, refetchInterval: 10000 });
  const [planDraft, setPlanDraft] = React.useState<api.BackupPlanConfig | null>(null);
  const [planVersion, setPlanVersion] = React.useState("");
  const [planError, setPlanError] = React.useState("");
  const editPlan = () => {
    if (!version || !plan.data || lock.current || disabled) return;
    setPlanVersion(identity); setPlanDraft({ ...plan.data.config }); setPlanError("");
  };
  const savePlan = async (event: React.FormEvent) => {
    event.preventDefault(); if (lock.current || disabled || !planDraft || planVersion !== identity) return;
    lock.current = true; setBusy(true); setPlanError("");
    try { const result = engine === "postgresql" ? await api.postgresBackupPlanSave(version, planDraft) : await api.dbBackupPlanSave(engine, version, planDraft); queryClient.setQueryData(queryKey, result); setPlanDraft(null); toast.success(t("settings.saved")); }
    catch (error) { const parsed = normalizeError(error); setPlanError([parsed.message, parsed.hint].filter(Boolean).join(" ")); }
    finally { lock.current = false; setBusy(false); invalidate("database-backup-plan"); }
  };
  const runPlan = async () => {
    if (lock.current || disabled || !ready || plan.data?.state === "running") return;
    lock.current = true; setBusy(true); setError("");
    try { const result = engine === "postgresql" ? await api.postgresBackupPlanRun(version) : await api.dbBackupPlanRun(engine, version); queryClient.setQueryData(queryKey, result); if (result.state === "success") toast.success(t("dbBackup.done")); else if (result.state === "skipped") toast.info(result.message); else toast.error(result.message || t("pgSchedule.failed")); }
    catch (error) { fail(error); }
    finally { lock.current = false; setBusy(false); invalidate("database-backup-plan", filesKey); }
  };
  const planRunning = plan.data?.state === "running";
  const planState = busy && !planDraft ? "running" : plan.data?.state || "idle";
  const planStateKeys = { idle: "pgSchedule.idle", running: "pgSchedule.running", success: "pgSchedule.success", failed: "pgSchedule.failed", partial: "pgSchedule.partial", skipped: "pgSchedule.skipped", interrupted: "pgSchedule.interrupted" } as const;
  const planStateLabel = t(planStateKeys[planState as keyof typeof planStateKeys] ?? "pgSchedule.failed");
  React.useEffect(() => { onLockChange(busy || !!planDraft || planRunning); return () => onLockChange(false); }, [busy, planDraft, planRunning, onLockChange]);
  React.useEffect(() => { if (plan.data?.finishedAt) void queryClient.invalidateQueries({ queryKey: [filesKey] }); }, [plan.data?.finishedAt, filesKey, queryClient]);
  return <>
      {!!version && <div className="mb-4 space-y-3 border-b border-dashed border-border pb-4">
        <div className="flex flex-wrap items-center justify-between gap-2"><p className="flex min-w-0 flex-wrap items-center gap-2 text-sm font-medium"><CalendarClock className="h-4 w-4 shrink-0" />{t("pgSchedule.title")}<span className="text-xs font-normal text-muted">{t(plan.data?.config.enabled ? "pgSchedule.on" : "pgSchedule.off")}</span></p><div className="flex flex-wrap gap-2"><Button size="sm" variant="ghost" disabled={disabled || busy || planRunning || !plan.data} onClick={editPlan}>{t("pgSchedule.configure")}</Button><Button size="sm" variant="secondary" disabled={disabled || busy || planRunning || !ready || !plan.data} onClick={() => void runPlan()}>{t("pgSchedule.run")}</Button></div></div>
        {plan.isPending ? <p role="status" className="text-xs text-muted">{t("db.loading")}</p> : plan.isError ? <p role="alert" className="text-xs text-error">{normalizeError(plan.error).message}<Button size="sm" variant="ghost" onClick={() => void plan.refetch()}>{t("db.retry")}</Button></p> : <>
          <p className="text-xs leading-5 text-muted">{t(engine === "postgresql" ? "pgSchedule.scope" : "pgSchedule.sqlScope")}</p>
          <div className="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted"><p>{t("pgSchedule.next")}: {plan.data?.nextAt ? new Date(plan.data.nextAt).toLocaleString() : "—"}</p><p>{t("pgSchedule.keep")}: {plan.data?.config.keep === 0 ? t("pgSchedule.keepAll") : plan.data?.config.keep}</p><p>{t("pgSchedule.last")}: {plan.data?.lastRunAt ? new Date(plan.data.lastRunAt).toLocaleString() : "—"} · {planStateLabel}</p></div>
          {!!plan.data?.message && <p role={/failed|partial|interrupted/.test(planState) ? "alert" : "status"} className={`break-words text-xs ${/failed|partial|interrupted/.test(planState) ? "text-error" : "text-muted"}`}>{plan.data.message}</p>}
          {!!plan.data?.files.length && <details className="text-xs text-muted"><summary className="cursor-pointer">{t("pgSchedule.files")} ({plan.data.files.length})</summary><ul className="mt-2 max-h-48 space-y-1 overflow-y-auto pl-4">{plan.data.files.map((file) => <li key={file} className="break-all font-mono">{file}</li>)}</ul></details>}
        </>}
      </div>}
    {!!error && <p role="alert" className="mb-3 break-words text-xs text-error">{error}</p>}
    <Dialog open={!!planDraft} onOpenChange={(open) => { if (!open && !lock.current) setPlanDraft(null); }}><DialogContent hideClose={busy} className="flex max-w-xl max-h-[85dvh] flex-col overflow-hidden">
      <DialogHeader><DialogTitle>{t("pgSchedule.title")}</DialogTitle><DialogDescription>{targetLabel}</DialogDescription></DialogHeader>
      {planDraft && <form onSubmit={savePlan} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
        <p className="text-xs leading-5 text-muted">{t("pgSchedule.hint")}</p>
        <div className="flex items-center justify-between gap-3"><Label htmlFor={`${id}-enabled`}>{t("pgSchedule.enable")}</Label><Switch id={`${id}-enabled`} checked={planDraft.enabled} disabled={busy} onCheckedChange={(enabled) => setPlanDraft({ ...planDraft, enabled })} /></div>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2"><div className="space-y-1.5"><Label htmlFor={`${id}-frequency`}>{t("pgSchedule.frequency")}</Label><Select value={planDraft.frequency} disabled={busy || !planDraft.enabled} onValueChange={(frequency: api.BackupPlanConfig["frequency"]) => setPlanDraft({ ...planDraft, frequency })}><SelectTrigger id={`${id}-frequency`}><SelectValue /></SelectTrigger><SelectContent>{(["daily", "weekly", "monthly"] as const).map((value) => <SelectItem key={value} value={value}>{t(`pgSchedule.${value}`)}</SelectItem>)}</SelectContent></Select></div>
        <div className="space-y-1.5"><Label htmlFor={`${id}-time`}>{t("pgSchedule.time")}</Label><Input id={`${id}-time`} type="time" required value={planDraft.time} disabled={busy || !planDraft.enabled} onChange={(e) => setPlanDraft({ ...planDraft, time: e.target.value })} /></div></div>
        {planDraft.frequency === "weekly" && <div className="space-y-1.5"><Label htmlFor={`${id}-weekday`}>{t("pgSchedule.weekday")}</Label><Select value={String(planDraft.weekday)} disabled={busy || !planDraft.enabled} onValueChange={(value) => setPlanDraft({ ...planDraft, weekday: Number(value) })}><SelectTrigger id={`${id}-weekday`}><SelectValue /></SelectTrigger><SelectContent>{Array.from({ length: 7 }, (_, day) => <SelectItem key={day} value={String(day)}>{t(`pgSchedule.day${day}` as "pgSchedule.day0")}</SelectItem>)}</SelectContent></Select></div>}
        {planDraft.frequency === "monthly" && <div className="space-y-1.5"><Label htmlFor={`${id}-monthday`}>{t("pgSchedule.monthDay")}</Label><Select value={String(planDraft.monthDay)} disabled={busy || !planDraft.enabled} onValueChange={(value) => setPlanDraft({ ...planDraft, monthDay: Number(value) })}><SelectTrigger id={`${id}-monthday`}><SelectValue /></SelectTrigger><SelectContent>{Array.from({ length: 31 }, (_, i) => <SelectItem key={i} value={String(i + 1)}>{i + 1}</SelectItem>)}</SelectContent></Select><p className="text-xs text-muted">{t("pgSchedule.monthHint")}</p></div>}
        <div className="space-y-1.5"><Label htmlFor={`${id}-keep`}>{t("pgSchedule.keep")}</Label><Input id={`${id}-keep`} type="number" min={0} max={100} step={1} required disabled={busy} value={planDraft.keep} onChange={(e) => setPlanDraft({ ...planDraft, keep: Number(e.target.value) })} /><p className="text-xs leading-5 text-muted">{t("pgSchedule.keepHint")}</p></div>
        {(planError || planVersion !== identity) && <p role="alert" className="break-words text-sm text-error">{planError || t("db.pgChanged")}</p>}
      </div><DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={() => setPlanDraft(null)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || planVersion !== identity || !planDraft.time || !Number.isInteger(planDraft.keep) || planDraft.keep < 0 || planDraft.keep > 100}>{busy ? t("confirm.busy") : t("common.save")}</Button></DialogFooter></form>}
    </DialogContent></Dialog>
  </>;
}
