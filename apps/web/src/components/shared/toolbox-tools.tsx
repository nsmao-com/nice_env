"use client";

import * as React from "react";
import Link from "next/link";
import { toast } from "sonner";
import {
  CalendarClock,
  Globe2,
  Bot,
  Database,
  Play,
  Trash2,
  RefreshCw,
  Loader2,
  ExternalLink,
  Pencil,
  Square,
} from "lucide-react";
import type { CronJob, TunnelInfo, OllamaPullStatus } from "@/lib/api";
import { cn, fmtBytes } from "@/lib/utils";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem, SelectSeparator } from "@/components/ui/select";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { useAdminer, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { ConfirmDialog, CopyButton } from "@/components/shared/misc";

/* ============================================================
   工具箱扩展：计划任务（应用级 cron）/ 快速隧道（cloudflared）/
   Ollama 模型管理 / Adminer 数据库管理台。
   ============================================================ */

/** 与 tools 页 ToolCard 同款卡片骨架（本地副本，避免页面组件循环导入） */
function ToolCard({
  icon: Icon,
  title,
  hint,
  children,
}: {
  icon: React.ComponentType<{ className?: string; strokeWidth?: number }>;
  title: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <Card className="min-w-0">
      <CardHeader className="flex-row items-start gap-3 px-3 sm:px-5">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
          <Icon className="h-4 w-4 shrink-0 text-primary" strokeWidth={1.8} />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{title}</CardTitle>
          <CardDescription className="mt-0.5 line-clamp-2 text-[11px] leading-relaxed">{hint}</CardDescription>
        </div>
      </CardHeader>
      <CardContent className="px-3 sm:px-5">{children}</CardContent>
    </Card>
  );
}

function relTime(ts?: number | null): string {
  if (!ts) return "—";
  const diff = Date.now() - ts;
  if (diff < 60_000) return "<1m";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h`;
  return `${Math.floor(diff / 86_400_000)}d`;
}

/* ================= 计划任务 ================= */

export function CronTool() {
  const t = useT();
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["cron-jobs"], queryFn: api.cronJobs, refetchInterval: 2000, retry: false });
  const jobs = query.data ?? [];
  const [editing, setEditing] = React.useState<CronJob | null>(null);
  const [name, setName] = React.useState("");
  const [command, setCommand] = React.useState("");
  const [intervalMin, setIntervalMin] = React.useState("30");
  const [preset, setPreset] = React.useState("30");
  const [saving, setSaving] = React.useState(false);
  const saveGuard = React.useRef(false);
  const [saveError, setSaveError] = React.useState<string | null>(null);
  const [expanded, setExpanded] = React.useState<string | null>(null);
  const [deleting, setDeleting] = React.useState<CronJob | null>(null);
  const [working, setWorking] = React.useState<Record<string, string>>({});
  const actions = React.useRef(new Set<string>());
  const [runRequests, setRunRequests] = React.useState<string[]>([]);
  const runs = React.useRef(new Set<string>());
  const form = React.useRef<HTMLFormElement>(null);
  const nameId = React.useId();
  const presets = [1, 5, 15, 30, 60, 1440];
  const refresh = () => query.refetch();
  const reset = () => { setEditing(null); setName(""); setCommand(""); setSaveError(null); };
  const edit = (job: CronJob) => {
    setEditing(job); setName(job.name); setCommand(job.command); setIntervalMin(String(job.intervalMin));
    setPreset(presets.includes(job.intervalMin) ? String(job.intervalMin) : "custom"); setSaveError(null);
    form.current?.scrollIntoView({ block: "center", behavior: "smooth" });
    form.current?.querySelector<HTMLInputElement>("input")?.focus({ preventScroll: true });
  };
  const act = async (id: string, kind: string, action: () => Promise<unknown>) => {
    if (actions.current.has(id)) return;
    actions.current.add(id); setWorking((old) => ({ ...old, [id]: kind }));
    try { await action(); } catch (e) { toastError(e); }
    finally {
      await refresh(); actions.current.delete(id);
      setWorking((old) => { const next = { ...old }; delete next[id]; return next; });
    }
  };
  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    if (saveGuard.current) return;
    const minutes = Number(intervalMin);
    if (!name.trim() || !command.trim() || !Number.isInteger(minutes) || minutes < 1 || minutes > 525600) {
      setSaveError(t("tools.cron.invalid")); return;
    }
    saveGuard.current = true; setSaving(true); setSaveError(null);
    try {
      await api.cronSave({ id: editing?.id ?? "", name: name.trim(), command: command.trim(), intervalMin: minutes,
        enabled: editing?.enabled ?? true, createdAt: editing?.createdAt ?? Date.now() });
      reset(); toast.success(t("tools.cron.saved")); await refresh();
    } catch (e) { setSaveError(normalizeError(e).message); }
    finally { saveGuard.current = false; setSaving(false); }
  };
  const runNow = async (id: string) => {
    if (runs.current.has(id) || actions.current.has(id)) return;
    runs.current.add(id); setRunRequests([...runs.current]);
    try {
      const result = await api.cronRunNow(id);
      client.setQueryData<CronJob[]>(["cron-jobs"], (old) => old?.map((job) => job.id === id ? result : job));
      if (result.lastExit === "exit 0") toast.success(t("tools.cron.ranNow"));
      else if (result.lastExit === "cancelled") toast.message(t("tools.cron.cancelled"));
      else { toast.error(t("tools.cron.failed")); setExpanded(id); }
    } catch (e) { toastError(e); }
    finally { runs.current.delete(id); setRunRequests([...runs.current]); await refresh(); }
  };
  const status = (job: CronJob) => {
    if (job.lastExit === "running" || runRequests.includes(job.id)) return t("tools.cron.running");
    if (!job.lastExit) return t("tools.cron.neverRun");
    if (job.lastExit === "exit 0") return t("tools.cron.success");
    if (job.lastExit === "cancelled") return t("tools.cron.cancelled");
    if (job.lastExit === "interrupted") return t("tools.cron.interrupted");
    if (job.lastExit === "timeout") return t("tools.cron.timeout");
    return `${t("tools.cron.failed")} · ${job.lastExit}`;
  };

  return (
    <ToolCard icon={CalendarClock} title={t("tools.cron.title")} hint={t("tools.cron.hint")}>
      <div className="flex min-w-0 flex-col gap-3">
        {!isTauri && <p className="rounded-lg bg-info-soft p-3 text-xs text-info">{t("tools.cron.preview")}</p>}
        {query.error && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
          <span className="min-w-0 flex-1 break-words">{t("tools.cron.readFailed")} · {normalizeError(query.error).message}</span>
          <Button size="sm" variant="secondary" disabled={query.isFetching} onClick={() => void refresh()}>{t("install.retry")}</Button>
        </div>}
        {query.isPending ? <p role="status" className="py-4 text-center text-xs text-faint">{t("common.loading")}</p>
          : !query.error && jobs.length === 0 ? <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-faint">{t("tools.cron.empty")}</p> : null}
        {jobs.map((job) => {
          const running = job.lastExit === "running" || runRequests.includes(job.id);
          const pending = !!working[job.id] || (saving && editing?.id === job.id);
          const locked = pending || !!query.error;
          const next = (job.lastRunAt ?? job.createdAt) + job.intervalMin * 60000;
          return <div key={job.id} className="min-w-0 rounded-lg border border-border/60 p-3">
            <div className="flex min-w-0 items-start gap-2">
              <Switch className="mt-1 shrink-0" checked={job.enabled} disabled={locked} aria-label={`${t("tools.cron.schedule")} · ${job.name}`}
                onCheckedChange={(enabled) => void act(job.id, "toggle", () => api.cronSetEnabled(job.id, enabled))} />
              <div className="min-w-0 flex-1">
                <p className="break-words text-[12.5px] font-medium">{job.name}</p>
                <p className="mt-1 break-all font-mono text-[11px] text-faint">{job.command}</p>
              </div>
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-2 text-[11px]">
              <Badge variant="info">{t("tools.cron.every").replace("{n}", String(job.intervalMin))}</Badge>
              <span role="status" className={cn("break-words", running ? "text-warn" : job.lastExit === "exit 0" ? "text-running" : job.lastExit ? "text-error" : "text-faint")}>{status(job)}</span>
              {job.lastRunAt && <time title={new Date(job.lastRunAt).toLocaleString()} className="text-faint">{t("tools.cron.last")} {relTime(job.lastRunAt)}</time>}
            </div>
            <p className="mt-1 break-words text-[10.5px] text-faint">
              {!job.enabled ? t("tools.cron.paused") : running ? t("tools.cron.runningHint") : `${t("tools.cron.next")} ${next <= Date.now() ? t("tools.cron.due") : new Date(next).toLocaleString()}`}
            </p>
            <div className="mt-2 flex flex-wrap items-center gap-1 border-t border-dashed border-border pt-2">
              {running ? <Button size="sm" variant="secondary" disabled={pending} aria-label={`${t("common.stop")} · ${job.name}`}
                onClick={() => void act(job.id, "stop", async () => { await api.cronStop(job.id); toast.message(t("tools.cron.stopRequested")); })}>
                {working[job.id] === "stop" ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Square className="h-3.5 w-3.5" />} {t("common.stop")}
              </Button> : <Button size="sm" variant="secondary" disabled={locked} aria-label={`${t("tools.cron.runNow")} · ${job.name}`} onClick={() => void runNow(job.id)}><Play className="h-3.5 w-3.5" />{t("tools.cron.runNow")}</Button>}
              <Button size="sm" variant="ghost" disabled={locked || running || saving} aria-label={`${t("tools.cron.edit")} · ${job.name}`} onClick={() => edit(job)}><Pencil className="h-3.5 w-3.5" />{t("tools.cron.edit")}</Button>
              <Button size="sm" variant="ghost" aria-label={`${t("tools.cron.log")} · ${job.name}`} aria-expanded={expanded === job.id} onClick={() => setExpanded(expanded === job.id ? null : job.id)}>{t("tools.cron.log")}</Button>
              <Button size="icon-sm" variant="ghost" className="ml-auto text-faint hover:text-error" disabled={locked || running || saving} aria-label={`${t("common.delete")} · ${job.name}`} onClick={() => setDeleting(job)}><Trash2 className="h-3.5 w-3.5" /></Button>
            </div>
            {expanded === job.id && <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-card-2/50 p-3 font-mono text-[11px] leading-relaxed text-secondary">{running ? t("tools.cron.outputPending") : job.lastOutput || t("tools.cron.noOutput")}</pre>}
          </div>;
        })}
        <form ref={form} onSubmit={save} className="flex flex-col gap-3 rounded-lg border border-dashed border-border p-3">
          <p className="break-words text-xs font-medium">{editing ? `${t("tools.cron.edit")} · ${editing.name}` : t("tools.cron.new")}</p>
          <label htmlFor={nameId} className="space-y-1.5 text-xs text-muted">{t("tools.cron.nameLabel")}
            <Input id={nameId} value={name} disabled={saving} maxLength={128} onChange={(e) => setName(e.target.value)} placeholder={t("tools.cron.namePh")} required />
          </label>
          <label className="space-y-1.5 text-xs text-muted">{t("tools.cron.commandLabel")}
            <Input value={command} disabled={saving} maxLength={8192} onChange={(e) => setCommand(e.target.value)} placeholder={t("tools.cron.cmdPh")} className="font-mono text-xs" required />
          </label>
          <div className="space-y-1.5 text-xs text-muted">
            <p id={`${nameId}-interval`}>{t("tools.cron.interval")}</p>
            <div className="flex flex-wrap gap-2">
              <Select value={preset} disabled={saving} onValueChange={(value) => { setPreset(value); if (value !== "custom") setIntervalMin(value); }}>
                <SelectTrigger className="min-w-0 flex-1 basis-32" aria-labelledby={`${nameId}-interval`}><SelectValue /></SelectTrigger>
                <SelectContent>{presets.map((minutes) => <SelectItem key={minutes} value={String(minutes)}>{t("tools.cron.every").replace("{n}", String(minutes))}</SelectItem>)}<SelectItem value="custom">{t("tools.cron.custom")}</SelectItem></SelectContent>
              </Select>
              {preset === "custom" && <Input type="number" className="min-w-0 flex-1 basis-24" value={intervalMin} min={1} max={525600} step={1} disabled={saving} aria-label={t("tools.cron.customMinutes")} onChange={(e) => setIntervalMin(e.target.value)} required />}
            </div>
          </div>
          <p className="text-[11px] leading-relaxed text-faint">{t("tools.cron.hint2")}</p>
          {saveError && <p role="alert" className="break-words text-xs text-error">{saveError}</p>}
          <div className="flex flex-wrap justify-end gap-2">
            {editing && <Button type="button" variant="ghost" disabled={saving} onClick={reset}>{t("common.cancel")}</Button>}
            <Button type="submit" disabled={saving || !!query.error || (editing !== null && (runs.current.has(editing.id) || jobs.some((j) => j.id === editing.id && j.lastExit === "running")))}>
              {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{editing ? t("common.save") : t("common.add")}
            </Button>
          </div>
        </form>
      </div>
      <ConfirmDialog open={deleting !== null} loading={!!deleting && working[deleting.id] === "delete"}
        onOpenChange={(open) => { if (!open && (!deleting || !actions.current.has(deleting.id))) setDeleting(null); }}
        title={`${t("common.delete")} · ${deleting?.name ?? ""}`} description={t("tools.cron.deleteHint")} danger confirmText={t("common.delete")}
        onConfirm={async () => { if (!deleting) return; const target = deleting; await act(target.id, "delete", async () => { await api.cronDelete(target.id); setDeleting(null); if (editing?.id === target.id) reset(); }); }} />
    </ToolCard>
  );
}

/* ================= 快速隧道 ================= */

export function TunnelTool() {
  const t = useT();
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["tunnels"], queryFn: api.tunnelList, refetchInterval: 2500, retry: false });
  const sites = useQuery({ queryKey: ["tunnel-sites"], queryFn: api.listSites, refetchInterval: 5000, retry: false });
  const [target, setTarget] = React.useState("");
  const [port, setPort] = React.useState("8080");
  const [starting, setStarting] = React.useState(false);
  const startGuard = React.useRef(false);
  const [startError, setStartError] = React.useState<string | null>(null);
  const [working, setWorking] = React.useState<string[]>([]);
  const actions = React.useRef(new Set<string>());
  const [expanded, setExpanded] = React.useState<string | null>(null);
  const targetId = React.useId();
  const selectedSite = sites.data?.find((site) => `site:${site.id}` === target);
  const validTarget = target === "custom" || (!!selectedSite && selectedSite.status === "running" && !sites.error);
  const refresh = () => query.refetch();
  const start = async (retry?: TunnelInfo) => {
    if (startGuard.current) return;
    const siteId = retry ? retry.siteId : selectedSite?.id;
    const localPort = retry?.port ?? Number(port);
    if (!retry && !validTarget) return;
    if (!siteId && (!Number.isInteger(localPort) || localPort < 1 || localPort > 65535)) {
      setStartError(t("tools.tunnel.badPort")); return;
    }
    startGuard.current = true; setStarting(true); setStartError(null);
    try {
      const info = siteId ? await api.tunnelStartSite(siteId) : await api.tunnelStart(localPort);
      await client.cancelQueries({ queryKey: ["tunnels"] });
      client.setQueryData<TunnelInfo[]>(["tunnels"], (old) => [...(old ?? []).filter((row) => row.id !== info.id), info]);
      if (info.state === "failed") { setExpanded(info.id); toast.error(info.error || t("tools.tunnel.failed")); }
      else toast.message(t(info.state === "connected" ? "tools.tunnel.connected" : "tools.tunnel.starting"));
      await refresh();
    } catch (e) { setStartError(normalizeError(e).message); if (retry) toastError(e); }
    finally { startGuard.current = false; setStarting(false); }
  };
  const act = async (id: string, action: () => Promise<unknown>) => {
    if (actions.current.has(id)) return;
    actions.current.add(id); setWorking([...actions.current]);
    try { await action(); } catch (e) { toastError(e); }
    finally { await refresh(); actions.current.delete(id); setWorking([...actions.current]); }
  };

  return (
    <ToolCard icon={Globe2} title={t("tools.tunnel.title")} hint={t("tools.tunnel.hint")}>
      <div className="flex min-w-0 flex-col gap-3">
        {!isTauri && <p className="rounded-lg bg-info-soft p-3 text-xs text-info">{t("tools.tunnel.preview")}</p>}
        <form className="flex min-w-0 flex-col gap-2" onSubmit={(event) => { event.preventDefault(); void start(); }}>
          <label id={targetId} className="text-xs text-muted">{t("tools.tunnel.target")}</label>
          <Select value={target} onValueChange={(value) => { setTarget(value); setStartError(null); }} disabled={starting}>
            <SelectTrigger className="min-w-0 text-xs" aria-labelledby={targetId}><SelectValue placeholder={t("tools.tunnel.choose")} /></SelectTrigger>
            <SelectContent>
              {(sites.data ?? []).map((site) => <SelectItem key={site.id} value={`site:${site.id}`} disabled={site.status !== "running" || !!sites.error} className="text-xs [&>span:last-child]:min-w-0 [&>span:last-child]:break-all">
                {site.name} · {site.domains[0]}{site.status !== "running" ? ` · ${t("tools.tunnel.siteStopped")}` : ""}
              </SelectItem>)}
              {!!sites.data?.length && <SelectSeparator />}
              <SelectItem value="custom" className="text-xs">{t("tools.tunnel.custom")}</SelectItem>
            </SelectContent>
          </Select>
          {sites.isPending && <p role="status" className="text-[11px] text-faint">{t("tools.tunnel.loadingSites")}</p>}
          {sites.error && <div role="alert" className="flex flex-wrap items-center gap-2 text-xs text-error">
            <span className="min-w-0 flex-1 break-words">{t("tools.tunnel.sitesFailed")} · {normalizeError(sites.error).message}</span>
            <Button type="button" size="sm" variant="secondary" disabled={sites.isFetching} onClick={() => void sites.refetch()}>{t("install.retry")}</Button>
          </div>}
          {target === "custom" && <label className="space-y-1.5 text-xs text-muted">{t("tools.tunnel.port")}
            <Input type="number" min={1} max={65535} step={1} required value={port} disabled={starting} onChange={(event) => setPort(event.target.value)} className="font-mono text-xs" />
          </label>}
          <p className="text-[11px] leading-relaxed text-faint">{t("tools.tunnel.targetHint")}</p>
          {startError && <p role="alert" className="break-words text-xs text-error">{startError}</p>}
          <Button type="submit" size="sm" className="self-start" disabled={starting || query.isPending || !!query.error || !validTarget}>
            {starting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Globe2 className="h-3.5 w-3.5" />}{t("tools.tunnel.start")}
          </Button>
        </form>
        {query.error && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
          <span className="min-w-0 flex-1 break-words">{t("tools.tunnel.readFailed")} · {normalizeError(query.error).message}</span>
          <Button size="sm" variant="secondary" disabled={query.isFetching} onClick={() => void refresh()}>{t("install.retry")}</Button>
        </div>}
        {query.isPending ? <p role="status" className="py-4 text-center text-xs text-faint">{t("common.loading")}</p>
          : !query.error && !query.data?.length ? <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-faint">{t("tools.tunnel.none")}</p> : null}
        {(query.data ?? []).map((tn) => <section key={tn.id} aria-label={tn.target} className="min-w-0 rounded-lg border border-border/60 p-3">
          <p className="break-all font-mono text-xs">{tn.target}</p>
          <div className="mt-2 flex flex-wrap items-center gap-2 text-[11px]">
            <Badge role="status" variant={tn.state === "connected" ? "running" : tn.state === "failed" ? "error" : "info"}>{t(`tools.tunnel.${tn.state}`)}</Badge>
            <time className="text-faint" title={new Date(tn.startedAt).toLocaleString()}>{relTime(tn.startedAt)}</time>
          </div>
          {tn.alive && tn.localReachable === false && <p role="status" className="mt-2 text-xs text-warn">{t("tools.tunnel.localDown")}</p>}
          {tn.error && <p role="alert" className="mt-2 break-words text-xs text-error">{tn.error}</p>}
          {tn.url ? <p className="mt-2 break-all font-mono text-[11px] text-secondary">{tn.url}</p>
            : tn.alive && <p className="mt-2 text-[11px] text-faint">{t("tools.tunnel.pending")}</p>}
          <div className="mt-2 flex flex-wrap items-center gap-1 border-t border-dashed border-border pt-2">
            {tn.url && <>
              <CopyButton text={tn.url} />
              <Button size="sm" variant="ghost" disabled={!isTauri || !!query.error || tn.state !== "connected" || !tn.localReachable}
                onClick={() => void api.openInBrowser(tn.url!).catch(toastError)}><ExternalLink className="h-3.5 w-3.5" />{t("common.open")}</Button>
            </>}
            <Button size="sm" variant="ghost" aria-expanded={expanded === tn.id} onClick={() => setExpanded(expanded === tn.id ? null : tn.id)}>{t("tools.tunnel.output")}</Button>
            {tn.alive ? <Button size="sm" variant="secondary" disabled={working.includes(tn.id)} onClick={() => void act(tn.id, () => api.tunnelStop(tn.id))}>
              {working.includes(tn.id) ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Square className="h-3.5 w-3.5" />}{t("tools.tunnel.stop")}
            </Button> : <>
              <Button size="sm" variant="secondary" disabled={starting || working.includes(tn.id) || !!query.error} onClick={() => void start(tn)}>{t("tools.tunnel.restart")}</Button>
              <Button size="icon-sm" variant="ghost" className="ml-auto text-faint hover:text-error" aria-label={`${t("tools.tunnel.remove")} · ${tn.target}`} disabled={working.includes(tn.id) || !!query.error} onClick={() => void act(tn.id, () => api.tunnelRemove(tn.id))}><Trash2 className="h-3.5 w-3.5" /></Button>
            </>}
          </div>
          {expanded === tn.id && <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-card-2/50 p-3 font-mono text-[11px] leading-relaxed text-secondary">{tn.logs.join("\n") || t("tools.tunnel.noOutput")}</pre>}
        </section>)}
        <p className="text-[11px] leading-relaxed text-faint">{t("tools.tunnel.hint2")}</p>
      </div>
    </ToolCard>
  );
}

/* ================= Ollama 模型管理 ================= */

export function OllamaTool() {
  const t = useT();
  const client = useQueryClient();
  const models = useQuery({ queryKey: ["ollama-models"], queryFn: api.ollamaModels, refetchInterval: 5000, retry: false });
  const task = useQuery({ queryKey: ["ollama-pull"], queryFn: api.ollamaPullStatus, refetchInterval: 1000, retry: false });
  const job = task.data;
  const active = job?.state === "pulling" || job?.state === "cancelling";
  const [pullName, setPullName] = React.useState("");
  const [preset, setPreset] = React.useState("custom");
  const [search, setSearch] = React.useState("");
  const [deleting, setDeleting] = React.useState<string | null>(null);
  const [action, setAction] = React.useState<string | null>(null);
  const guard = React.useRef(false);
  const [pullError, setPullError] = React.useState<string | null>(null);
  const inputId = React.useId();
  const presets = ["qwen3:0.6b", "gemma3:1b", "llama3.2:1b"];
  const rows = (models.data ?? []).filter((m) => m.name.toLowerCase().includes(search.trim().toLowerCase()));
  const locked = !!action || active || !!task.error || task.isPending;
  const refresh = () => Promise.all([models.refetch(), task.refetch()]);
  React.useEffect(() => {
    if (job?.endedAt) void client.invalidateQueries({ queryKey: ["ollama-models"] });
  }, [job?.id, job?.endedAt, client]);
  const pull = async (name = pullName) => {
    if (guard.current || active) return;
    guard.current = true; setAction("pull"); setPullError(null);
    try {
      const next = await api.ollamaPull(name.trim());
      await client.cancelQueries({ queryKey: ["ollama-pull"] });
      client.setQueryData<OllamaPullStatus>(["ollama-pull"], next);
      if (next.state === "failed") setPullError(next.error || t("tools.ollama.failed"));
    } catch (error) { setPullError(normalizeError(error).message); toastError(error); }
    finally { guard.current = false; setAction(null); }
  };
  const cancel = async () => {
    if (!job || guard.current) return;
    guard.current = true; setAction("cancel");
    try { await api.ollamaCancelPull(job.id); await task.refetch(); }
    catch (error) { toastError(error); }
    finally { guard.current = false; setAction(null); }
  };
  const remove = async () => {
    if (!deleting || guard.current) return;
    guard.current = true; setAction("delete");
    try {
      await api.ollamaDelete(deleting);
      setDeleting(null); toast.success(t("tools.ollama.deleted")); await models.refetch();
    } catch (error) { toastError(error); }
    finally { guard.current = false; setAction(null); }
  };
  const phase = (value: string) => {
    if (value === "pulling manifest") return t("tools.ollama.manifest");
    if (value.startsWith("pulling ")) return t("tools.ollama.downloading");
    if (value.startsWith("verifying")) return t("tools.ollama.verifying");
    if (value.startsWith("writing") || value.startsWith("removing")) return t("tools.ollama.saving");
    if (value === "checking local model") return t("tools.ollama.checking");
    return value;
  };
  const percent = job?.total && job.completed != null ? Math.min(100, Math.round(job.completed / job.total * 100)) : undefined;

  return <ToolCard icon={Bot} title={t("tools.ollama.title")} hint={t("tools.ollama.hint")}>
    <div className="flex min-w-0 flex-col gap-3">
      {!isTauri && <p className="rounded-lg bg-info-soft p-3 text-xs text-info">{t("tools.ollama.preview")}</p>}
      <div className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant="secondary" asChild><Link href="/packages">{t("tools.ollama.manageService")}</Link></Button>
        <Button size="sm" variant="ghost" onClick={() => void api.openInBrowser("https://ollama.com/library").catch(toastError)}><ExternalLink className="h-3.5 w-3.5" />{t("tools.ollama.library")}</Button>
        <Button size="icon-sm" variant="ghost" className="ml-auto" aria-label={t("tools.ollama.refresh")} disabled={models.isFetching || task.isFetching} onClick={() => void refresh()}><RefreshCw className={cn("h-3.5 w-3.5", models.isFetching && "animate-spin")} /></Button>
      </div>
      {models.error && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
        <span className="min-w-0 flex-1 break-words">{t("tools.ollama.readFailed")} · {normalizeError(models.error).message}</span>
        <Button size="sm" variant="secondary" disabled={models.isFetching} onClick={() => void models.refetch()}>{t("install.retry")}</Button>
      </div>}
      {task.error && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
        <span className="min-w-0 flex-1 break-words">{t("tools.ollama.taskFailed")} · {normalizeError(task.error).message}</span>
        <Button size="sm" variant="secondary" disabled={task.isFetching} onClick={() => void task.refetch()}>{t("install.retry")}</Button>
      </div>}
      <form className="flex min-w-0 flex-col gap-2" onSubmit={(event) => { event.preventDefault(); void pull(); }}>
        <p id={`${inputId}-presets`} className="text-xs text-muted">{t("tools.ollama.presets")}</p>
        <Select value={preset} disabled={locked} onValueChange={(value) => { setPreset(value); setPullError(null); if (value !== "custom") setPullName(value); }}>
          <SelectTrigger className="min-w-0 text-xs" aria-labelledby={`${inputId}-presets`}><SelectValue /></SelectTrigger>
          <SelectContent>{presets.map((name) => <SelectItem key={name} value={name} className="text-xs">{name}</SelectItem>)}<SelectSeparator /><SelectItem value="custom">{t("tools.ollama.custom")}</SelectItem></SelectContent>
        </Select>
        <label htmlFor={inputId} className="text-xs text-muted">{t("tools.ollama.nameLabel")}</label>
        <Input id={inputId} value={pullName} maxLength={255} disabled={locked} onChange={(event) => { setPullName(event.target.value); setPreset("custom"); }} placeholder={t("tools.ollama.pullPh")} className="min-w-0 font-mono text-xs" required />
        {pullError && <p role="alert" className="break-words text-xs text-error">{pullError}</p>}
        <Button type="submit" size="sm" className="self-start" disabled={locked || !!models.error || models.isPending || !pullName.trim()}>
          {action === "pull" && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{t("tools.ollama.pull")}
        </Button>
      </form>
      {job && <section aria-label={t("tools.ollama.task")} className="min-w-0 rounded-lg border border-border/60 p-3">
        <div className="flex flex-wrap items-center gap-2">
          <p className="min-w-0 flex-1 basis-40 break-all font-mono text-xs">{job.name}</p>
          <Badge role="status" variant={job.state === "succeeded" ? "running" : job.state === "failed" ? "error" : "info"}>{t(`tools.ollama.${job.state}`)}</Badge>
        </div>
        {active && <>
          <p className="mt-2 break-words text-[11px] text-muted">{phase(job.phase)}</p>
          {job.total != null && job.total > 0 && <div className="mt-2 space-y-1">
            <progress className="h-2 w-full accent-primary" aria-label={t("tools.ollama.layerProgress")} max={job.total} value={job.completed ?? undefined} />
            <p className="text-[11px] text-faint">{t("tools.ollama.layerProgress")} · {job.completed == null ? "—" : fmtBytes(job.completed)} / {fmtBytes(job.total)}{percent == null ? "" : ` · ${percent}%`}</p>
          </div>}
        </>}
        {job.error && <p role="alert" className="mt-2 break-words text-xs text-error">{job.error}</p>}
        <div className="mt-2 flex flex-wrap items-center gap-2 border-t border-dashed border-border pt-2">
          {active ? <Button size="sm" variant="secondary" disabled={!!action || job.state === "cancelling"} onClick={() => void cancel()}><Square className="h-3.5 w-3.5" />{t("tools.ollama.cancelPull")}</Button>
            : job.state !== "succeeded" && <Button size="sm" variant="secondary" disabled={locked || !!models.error} onClick={() => void pull(job.name)}>{t("install.retry")}</Button>}
          <span className="min-w-0 break-words text-[11px] text-faint">{new Date(job.endedAt ?? job.startedAt).toLocaleString()}</span>
        </div>
      </section>}
      <details className="text-[11px] leading-relaxed text-faint">
        <summary className="cursor-pointer rounded focus-visible:outline focus-visible:outline-primary">{t("tools.ollama.instructions")}</summary>
        <p className="mt-2">{t("tools.ollama.pullHint")}</p>
      </details>
      <div className="flex flex-col gap-2 border-t border-dashed border-border pt-3">
        <p className="text-xs font-medium">{t("tools.ollama.localModels")}{models.data ? ` · ${models.data.length}` : ""}</p>
        {!!models.data?.length && <Input value={search} aria-label={t("tools.ollama.search")} placeholder={t("tools.ollama.search")} onChange={(e) => setSearch(e.target.value)} className="text-xs" />}
        {models.isPending ? <p role="status" className="py-3 text-center text-xs text-faint">{t("common.loading")}</p>
          : !models.error && rows.length === 0 ? <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-faint">{t(models.data?.length ? "tools.ollama.noMatch" : "tools.ollama.none")}</p> : null}
        {rows.map((m) => <div key={m.name} className="min-w-0 rounded-lg border border-border/60 p-3">
          <p className="break-all font-mono text-xs font-medium">{m.name}</p>
          <p className="mt-1 break-words text-[11px] text-muted">{[fmtBytes(m.size), m.parameters, m.quantization].filter(Boolean).join(" · ")}</p>
          <p className="mt-1 break-words text-[10px] text-faint">{t("tools.ollama.modified")} {Number.isNaN(Date.parse(m.modified)) ? m.modified : new Date(m.modified).toLocaleString()}</p>
          <div className="mt-2 flex flex-wrap items-center gap-1 border-t border-dashed border-border pt-2">
            <CopyButton text={m.name} />
            <Button size="sm" variant="ghost" className="px-2" disabled={locked || !!models.error} onClick={() => void pull(m.name)}><RefreshCw className="h-3.5 w-3.5" />{t("tools.ollama.update")}</Button>
            <Button size="icon-sm" variant="ghost" className="ml-auto text-faint hover:text-error" disabled={locked || !!models.error} aria-label={`${t("tools.ollama.delete")} · ${m.name}`} onClick={() => setDeleting(m.name)}><Trash2 className="h-3.5 w-3.5" /></Button>
          </div>
        </div>)}
      </div>
    </div>
    <ConfirmDialog open={deleting !== null} loading={action === "delete"} confirmDisabled={active || !!task.error || !!models.error}
      onOpenChange={(open) => { if (!open && action !== "delete") setDeleting(null); }}
      title={`${t("tools.ollama.delete")} · ${deleting ?? ""}`} description={t("tools.ollama.deleteHint")} confirmText={t("common.delete")} danger onConfirm={() => void remove()} />
  </ToolCard>;
}

/* ================= Adminer 数据库管理台 ================= */

export function AdminerTool() {
  const t = useT();
  const { query, busy, open, stop } = useAdminer();
  const running = query.data;
  return (
    <ToolCard icon={Database} title={t("tools.adminer.title")} hint={t("tools.adminer.hint")}>
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1 basis-52 text-[11.5px] leading-relaxed text-muted">
          {query.isPending ? <p role="status">{t("common.loading")}</p> : query.isError ?
            <p role="alert">{t("tools.adminer.statusFailed")} <Button variant="ghost" size="sm" onClick={() => void query.refetch()}>{t("db.retry")}</Button></p> : running ? <>
              <p className="break-all font-mono text-running">{running.url}</p>
              <p>Adminer {running.adminerVersion} · PHP {running.phpVersion}</p>
            </> : <p>{t("tools.adminer.idle")}</p>}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant={running ? "secondary" : "default"} disabled={busy || query.isPending} onClick={() => void open()}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <ExternalLink className="h-3.5 w-3.5" />}
            {running ? t("common.open") : t("tools.adminer.start")}
          </Button>
          {(running || query.isError) && <Button size="sm" variant="ghost" className="text-error hover:text-error" disabled={busy} onClick={() => void stop()}>{t("common.stop")}</Button>}
        </div>
      </div>
    </ToolCard>
  );
}
