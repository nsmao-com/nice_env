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
} from "lucide-react";
import type { CronJob, TunnelInfo, OllamaModelRow } from "@/lib/api";
import { cn } from "@/lib/utils";
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
    <Card>
      <CardHeader className="flex-row items-start gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
          <Icon className="h-4 w-4 shrink-0 text-primary" strokeWidth={1.8} />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{title}</CardTitle>
          <CardDescription className="mt-0.5 line-clamp-2 text-[11px] leading-relaxed">{hint}</CardDescription>
        </div>
      </CardHeader>
      <CardContent>{children}</CardContent>
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
  const [jobs, setJobs] = React.useState<api.CronJob[]>([]);
  const [name, setName] = React.useState("");
  const [command, setCommand] = React.useState("");
  const [intervalMin, setIntervalMin] = React.useState("30");
  const [busy, setBusy] = React.useState(false);
  const [expanded, setExpanded] = React.useState<string | null>(null);

  const load = React.useCallback(async () => {
    try {
      setJobs(await api.cronJobs());
    } catch {
      /* ignore */
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  const add = async () => {
    if (!name.trim() || !command.trim()) return;
    setBusy(true);
    try {
      await api.cronSave({
        id: "",
        name: name.trim(),
        command: command.trim(),
        intervalMin: Math.max(1, Number(intervalMin) || 30),
        enabled: true,
        createdAt: Date.now(),
      });
      setName("");
      setCommand("");
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const runNow = async (id: string) => {
    setBusy(true);
    try {
      await api.cronRunNow(id);
      toast.success(t("tools.cron.ranNow"));
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <ToolCard icon={CalendarClock} title={t("tools.cron.title")} hint={t("tools.cron.hint")}>
      <div className="flex flex-col gap-2">
        {jobs.map((j) => (
          <div key={j.id} className="rounded-lg border border-border/60 px-3 py-2">
            <div className="flex items-center gap-2">
              <Switch
                checked={j.enabled}
                onCheckedChange={async (v) => {
                  try {
                    await api.cronSetEnabled(j.id, v);
                    await load();
                  } catch (e) {
                    toastError(e);
                  }
                }}
              />
              <div className="min-w-0 flex-1">
                <p className="truncate text-[12.5px] font-medium">{j.name}</p>
                <p className="truncate font-mono text-[10.5px] text-faint">{j.command}</p>
              </div>
              <Badge variant="info" className="shrink-0">
                {t("tools.cron.every").replace("{n}", String(j.intervalMin))}
              </Badge>
              <span
                className={cn(
                  "shrink-0 text-[10px]",
                  j.lastExit === "running"
                    ? "text-warn"
                    : j.lastExit == null
                      ? "text-faint"
                      : j.lastExit === "exit 0"
                        ? "text-running"
                        : "text-error"
                )}
                title={j.lastExit ?? ""}
              >
                {j.lastExit === "running"
                  ? t("tools.cron.running")
                  : j.lastExit == null
                    ? "—"
                    : `${t("tools.cron.last")} ${relTime(j.lastRunAt)} · ${j.lastExit}`}
              </span>
              <Button
                size="icon-sm"
                variant="ghost"
                className="shrink-0 text-faint hover:text-secondary"
                title={t("tools.cron.runNow")}
                disabled={busy || j.lastExit === "running"}
                onClick={() => void runNow(j.id)}
              >
                {j.lastExit === "running" ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Play className="h-3.5 w-3.5" />
                )}
              </Button>
              <Button
                size="icon-sm"
                variant="ghost"
                className="shrink-0 text-faint hover:text-secondary"
                onClick={() => setExpanded(expanded === j.id ? null : j.id)}
              >
                <span className="text-[10px]">{t("tools.cron.log")}</span>
              </Button>
              <Button
                size="icon-sm"
                variant="ghost"
                className="shrink-0 text-faint hover:text-error"
                onClick={async () => {
                  try {
                    await api.cronDelete(j.id);
                    await load();
                  } catch (e) {
                    toastError(e);
                  }
                }}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </div>
            {expanded === j.id && (
              <pre className="mt-2 max-h-40 overflow-auto rounded-md bg-card-2/50 p-2 font-mono text-[10.5px] leading-relaxed text-secondary">
                {j.lastOutput || t("tools.cron.noOutput")}
              </pre>
            )}
          </div>
        ))}

        {/* 新建 */}
        <div className="flex flex-col gap-1.5 rounded-lg border border-dashed border-border p-2.5">
          <div className="flex gap-1.5">
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("tools.cron.namePh")}
              className="h-7 flex-1 text-[11.5px]"
            />
            <Input
              value={intervalMin}
              onChange={(e) => setIntervalMin(e.target.value.replace(/\D/g, ""))}
              className="h-7 w-16 text-center font-mono text-[11.5px]"
              title={t("tools.cron.interval")}
            />
            <span className="self-center text-[10px] text-faint">{t("tools.cron.minUnit")}</span>
          </div>
          <div className="flex gap-1.5">
            <Input
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void add()}
              placeholder={t("tools.cron.cmdPh")}
              className="h-7 flex-1 font-mono text-[11px]"
            />
            <Button size="sm" onClick={() => void add()} disabled={busy || !name.trim() || !command.trim()}>
              {t("common.add")}
            </Button>
          </div>
          <p className="text-[10px] text-faint">{t("tools.cron.hint2")}</p>
        </div>
      </div>
    </ToolCard>
  );
}

/* ================= 快速隧道 ================= */

export function TunnelTool() {
  const t = useT();
  const [port, setPort] = React.useState("8080");
  const [tunnels, setTunnels] = React.useState<TunnelInfo[]>([]);
  const [busy, setBusy] = React.useState(false);

  const load = React.useCallback(async () => {
    try {
      setTunnels(await api.tunnelList());
    } catch {
      /* ignore */
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
        {tunnels.length === 0 ? (
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

  const load = React.useCallback(async () => {
    setLoading(true);
    try {
      setModels(await api.ollamaModels());
    } catch (e) {
      setModels([]);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => {
    load().catch(() => undefined);
  }, [load]);

  return (
    <ToolCard icon={Bot} title={t("tools.ollama.title")} hint={t("tools.ollama.hint")}>
      <div className="flex flex-col gap-2">
        <div className="flex gap-1.5">
          <Input
            value={pullName}
            onChange={(e) => setPullName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && pullName.trim() && void (async () => {
              try {
                setPulling(pullName.trim());
                await api.ollamaPull(pullName.trim());
                toast.info(t("tools.ollama.pullStarted").replace("{name}", pullName.trim()));
                setPullName("");
              } catch (err) {
                toastError(err);
              } finally {
                setPulling(null);
              }
            })()}
            placeholder={t("tools.ollama.pullPh")}
            className="h-8 flex-1 font-mono text-[11.5px]"
          />
          <Button
            size="sm"
            className="h-8"
            disabled={!pullName.trim() || pulling != null}
            onClick={async () => {
              try {
                setPulling(pullName.trim());
                await api.ollamaPull(pullName.trim());
                toast.info(t("tools.ollama.pullStarted").replace("{name}", pullName.trim()));
                setPullName("");
              } catch (err) {
                toastError(err);
              } finally {
                setPulling(null);
              }
            }}
          >
            {pulling ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
            {t("tools.ollama.pull")}
          </Button>
          <Button size="sm" variant="ghost" className="h-8" onClick={() => void load().catch(() => undefined)}>
            <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
          </Button>
        </div>

        {models.length === 0 ? (
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
