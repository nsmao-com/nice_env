"use client";

import * as React from "react";
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
import type { CronJob, TunnelInfo, OllamaModelRow } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "@/components/ui/select";
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
  const [port, setPort] = React.useState("8080");
  const [tunnels, setTunnels] = React.useState<TunnelInfo[]>([]);
  const [busy, setBusy] = React.useState(false);
  const [loadError, setLoadError] = React.useState<string | null>(null);

  const load = React.useCallback(async () => {
    try {
      setTunnels(await api.tunnelList());
      setLoadError(null);
    } catch (error) {
      setLoadError(normalizeError(error).message);
    }
  }, []);

  React.useEffect(() => {
    void load();
    const id = setInterval(load, 2500);
    return () => clearInterval(id);
  }, [load]);

  const start = async () => {
    const p = Number(port);
    if (!p || p < 1 || p > 65535) {
      toast.error(t("tools.tunnel.badPort"));
      return;
    }
    setBusy(true);
    try {
      await api.tunnelStart(p);
      toast.info(t("tools.tunnel.starting"));
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <ToolCard icon={Globe2} title={t("tools.tunnel.title")} hint={t("tools.tunnel.hint")}>
      <div className="flex flex-col gap-2">
        {loadError && (
          <div className="flex flex-wrap items-center gap-2 rounded-md border border-error/25 bg-error-soft/40 px-3 py-2 text-[11px] text-error" role="alert">
            <span className="min-w-0 flex-1 break-words">{loadError}</span>
            <Button size="sm" variant="secondary" onClick={() => void load()}>{t("install.retry")}</Button>
          </div>
        )}
        <div className="flex gap-1.5">
          <Input
            value={port}
            onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))}
            className="h-8 w-24 text-center font-mono text-[12px]"
            placeholder="8080"
          />
          <Button size="sm" className="h-8" onClick={() => void start()} disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Globe2 className="h-3.5 w-3.5" />}
            {t("tools.tunnel.start")}
          </Button>
        </div>
        {!loadError && tunnels.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-[11px] text-faint">
            {t("tools.tunnel.none")}
          </p>
        ) : (
          tunnels.map((tn) => (
            <div key={tn.id} className="rounded-lg border border-border/60 px-3 py-2">
              <div className="flex items-center gap-2">
                <Badge variant="info">:{tn.port}</Badge>
                {tn.url ? (
                  <>
                    <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-running">{tn.url}</span>
                    <CopyButton text={tn.url} />
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      className="text-faint hover:text-secondary"
                      title={t("common.open")}
                      onClick={() => void api.openInBrowser(tn.url!).catch(() => undefined)}
                    >
                      <ExternalLink className="h-3.5 w-3.5" />
                    </Button>
                  </>
                ) : (
                  <span className="flex flex-1 items-center gap-1.5 text-[11px] text-warn">
                    <Loader2 className="h-3 w-3 animate-spin" /> {t("tools.tunnel.pending")}
                  </span>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  className="shrink-0 text-error hover:text-error"
                  onClick={async () => {
                    try {
                      await api.tunnelStop(tn.id);
                      await load();
                    } catch (e) {
                      toastError(e);
                    }
                  }}
                >
                  {t("tools.tunnel.stop")}
                </Button>
              </div>
            </div>
          ))
        )}
        <p className="text-[10px] text-faint">{t("tools.tunnel.hint2")}</p>
      </div>
    </ToolCard>
  );
}

/* ================= Ollama 模型管理 ================= */

export function OllamaTool() {
  const t = useT();
  const [models, setModels] = React.useState<OllamaModelRow[]>([]);
  const [pullName, setPullName] = React.useState("");
  const [pulling, setPulling] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [deleting, setDeleting] = React.useState<string | null>(null);
  const [loadError, setLoadError] = React.useState<string | null>(null);

  const load = React.useCallback(async () => {
    setLoading(true);
    try {
      setModels(await api.ollamaModels());
      setLoadError(null);
    } catch (e) {
      setModels([]);
      setLoadError(normalizeError(e).message);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => {
    load().catch(() => undefined);
  }, [load]);

  const pull = async () => {
    const name = pullName.trim();
    if (!name || pulling != null) return;
    setPulling(name);
    try {
      await api.ollamaPull(name);
      toast.success(t("tools.ollama.pullStarted").replace("{name}", name));
      setPullName("");
      await load();
    } catch (err) {
      toastError(err);
    } finally {
      setPulling(null);
    }
  };

  return (
    <ToolCard icon={Bot} title={t("tools.ollama.title")} hint={t("tools.ollama.hint")}>
      <div className="flex flex-col gap-2">
        {loadError && (
          <div className="flex flex-wrap items-center gap-2 rounded-md border border-error/25 bg-error-soft/40 px-3 py-2 text-[11px] text-error" role="alert">
            <span className="min-w-0 flex-1 break-words">{loadError}</span>
            <Button size="sm" variant="secondary" onClick={() => void load().catch(() => undefined)}>{t("install.retry")}</Button>
          </div>
        )}
        <div className="flex gap-1.5">
          <Input
            value={pullName}
            onChange={(e) => setPullName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void pull()}
            placeholder={t("tools.ollama.pullPh")}
            className="h-8 flex-1 font-mono text-[11.5px]"
          />
          <Button
            size="sm"
            className="h-8"
            disabled={!pullName.trim() || pulling != null}
            onClick={() => void pull()}
          >
            {pulling ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
            {t("tools.ollama.pull")}
          </Button>
          <Button size="sm" variant="ghost" className="h-8" onClick={() => void load().catch(() => undefined)}>
            <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
          </Button>
        </div>

        {!loadError && models.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-[11px] text-faint">
            {t("tools.ollama.none")}
          </p>
        ) : (
          models.map((m) => (
            <div key={m.name} className="flex items-center gap-2.5 rounded-lg border border-border/60 px-3 py-2">
              <div className="min-w-0 flex-1">
                <p className="truncate font-mono text-[12px] font-medium">{m.name}</p>
                <p className="text-[10px] text-faint">
                  {m.size} · {t("tools.ollama.modified")} {m.modified}
                </p>
              </div>
              <Button
                size="icon-sm"
                variant="ghost"
                className="text-faint hover:text-error"
                onClick={() => setDeleting(m.name)}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </div>
          ))
        )}
      </div>

      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={`${t("tools.ollama.delete")} ${deleting ?? ""}`}
        description={t("tools.ollama.deleteHint")}
        confirmText={t("common.delete")}
        danger
        onConfirm={async () => {
          if (!deleting) return;
          try {
            await api.ollamaDelete(deleting);
            toast.success(`${deleting} ${t("common.deleted")}`);
            await load().catch(() => undefined);
          } catch (e) {
            toastError(e);
          } finally {
            setDeleting(null);
          }
        }}
      />
    </ToolCard>
  );
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
