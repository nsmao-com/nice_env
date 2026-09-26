"use client";

import * as React from "react";
import { toast } from "sonner";
import { useQuery } from "@tanstack/react-query";
import {
  Waypoints,
  Plus,
  Trash2,
  Gauge,
  Check,
  MonitorUp,
  RadioTower,
  Signal,
  SignalZero,
  Link2,
  RefreshCw,
  ArrowDownUp,
  Activity,
  ChevronDown,
  Zap,
  Loader2,
} from "lucide-react";
import type { ProxyGroupView } from "@nsb/schema";
import { cn, fmtBytes } from "@/lib/utils";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import { isTauri, normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { StatusLight } from "@/components/shared/status-light";
import { ConfirmDialog } from "@/components/shared/misc";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from "@/components/ui/dialog";
import { PageHeader } from "@/components/layout/app-shell";

/* 各数据源独立读取；失败保留上次结果，轮询由查询层去重。 */
function useProxyState() {
  const state = useQuery({ queryKey: ["proxy-status"], queryFn: api.proxyStatus, refetchInterval: 3000, retry: false });
  const subscriptions = useQuery({ queryKey: ["proxy-profiles"], queryFn: api.proxyProfiles, refetchInterval: 3000, retry: false });
  const activeId = subscriptions.data?.find((p) => p.active)?.id;
  const nodes = useQuery({ queryKey: ["proxy-nodes", activeId], queryFn: api.proxyNodes,
    enabled: !!state.data?.running && !state.error, refetchInterval: 3000, retry: false });
  const refresh = React.useCallback(async () => {
    await Promise.all([state.refetch(), subscriptions.refetch(), ...(state.data?.running ? [nodes.refetch()] : [])]);
  }, [state.refetch, subscriptions.refetch, nodes.refetch, state.data?.running]);
  const failure = state.error ?? subscriptions.error ?? (state.data?.running ? nodes.error : null);
  return { status: state.data, groups: state.data?.running ? nodes.data ?? [] : [], profiles: subscriptions.data ?? [],
    refresh, loading: state.isPending || subscriptions.isPending, refreshing: state.isFetching || subscriptions.isFetching || nodes.isFetching,
    profilesLoading: subscriptions.isPending, profilesError: !!subscriptions.error, nodesLoading: nodes.isPending,
    nodesError: !!nodes.error, error: failure ? normalizeError(failure).message : null };
}

function subscriptionLabel(url: string): string {
  if (url.startsWith("builtin:")) return "NiceEnv";
  try { return new URL(url).host; } catch { return "—"; }
}

export default function ProxyPage() {
  const t = useT();
  const { status, groups, profiles, refresh, loading, refreshing, profilesLoading, profilesError, nodesLoading, nodesError, error } = useProxyState();
  const [importOpen, setImportOpen] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [deleting, setDeleting] = React.useState<{ id: string; name: string } | null>(null);
  const [profileAction, setProfileAction] = React.useState<{ id: string; kind: "activate" | "update" | "delete" } | null>(null);
  const actionRef = React.useRef(false);
  const locked = busy || loading || !!error;
  const perform = async (work: () => Promise<void>) => {
    if (actionRef.current) return;
    actionRef.current = true;
    setBusy(true);
    try { await work(); } catch (e) { toastError(e); }
    finally { await refresh(); setBusy(false); actionRef.current = false; setProfileAction(null); }
  };
  const changeProfile = (id: string, name: string, kind: "activate" | "update") => {
    if (actionRef.current) return;
    setProfileAction({ id, kind });
    void perform(async () => {
      if (kind === "activate") await api.proxyActivateProfile(id);
      else await api.proxyUpdateProfile(id);
      toast.success(kind === "activate" ? `${t("proxy.switchedP1")} ${name}` : `${name} · ${t("proxy.subUpdated")}`);
    });
  };

  const running = status?.running ?? false;

  const toggleCore = (next: boolean) => void perform(async () => {
    if (next) await api.proxyStart(); else await api.proxyStop();
    toast.success(next ? t("proxy.coreStarted") : t("proxy.coreStopped"));
  });
  const toggleSystemProxy = (next: boolean) => void perform(async () => {
    if (next && !running) { toast.error(t("proxy.sysPrecondition")); return; }
    await api.proxySetSystem(next);
    toast.success(next ? `${t("proxy.sysOn")} → 127.0.0.1:${status?.mixedPort}` : t("proxy.sysOff"));
  });
  const setMode = (mode: "rule" | "global" | "direct") => void perform(async () => { await api.proxySetMode(mode); });

  return (
    <div className="pb-8">
      <PageHeader title={t("proxy.title")} subtitle={t("proxy.subtitle")} />

      {error && (
        <div role="alert" className="mb-4 flex flex-wrap items-center gap-3 rounded-xl border border-error/25 bg-error-soft/40 p-3 text-xs [overflow-wrap:anywhere]">
          <span className="min-w-0 flex-1 text-error">{t("proxy.readFailed")}：{error}</span>
          <Button size="sm" variant="secondary" disabled={refreshing} onClick={() => void refresh()}>
            <RefreshCw className={cn("h-3.5 w-3.5", refreshing && "animate-spin")} /> {t("proxy.retry")}
          </Button>
        </div>
      )}

      <div className="mb-6 grid grid-cols-1 gap-4 md:grid-cols-2">
        {/* 内核控制 */}
        <Card className={cn(running && "breath border-running/25")}>
          <CardHeader className="flex-row flex-wrap items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-fill">
                <Waypoints className={cn("h-5 w-5", running ? "text-running" : "text-faint")} strokeWidth={1.8} />
              </div>
              <div>
                <CardTitle className="flex items-center gap-2 text-[14px]">
                  mihomo <StatusLight state={!status || error ? "unknown" : running ? "running" : "stopped"} size={7} />
                </CardTitle>
                <CardDescription className="mt-1 break-all font-mono text-[11px]">
                  {status ? `${t("proxy.mixedPort")} ${status.mixedPort} · API ${status.controllerPort}${status.version ? ` · ${status.version}` : ""}` : t("common.loading")}
                </CardDescription>
              </div>
            </div>
            <Switch checked={running} onCheckedChange={toggleCore} disabled={busy || loading || !status || (!!error && !running)} aria-label={t("proxy.coreControl")} />
          </CardHeader>
          <CardContent>
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-[11.5px] text-muted">{t("proxy.mode")}</span>
              <div className="flex gap-1 rounded-lg bg-card-2/60 p-1">
                {(["rule", "global", "direct"] as const).map((m) => (
                  <button
                    key={m}
                    onClick={() => setMode(m)}
                    aria-pressed={status?.mode === m}
                    disabled={locked || !status}
                    className={cn(
                      "rounded-md px-2.5 py-1 text-[11.5px] font-medium transition-all",
                      status?.mode === m ? "bg-surface text-foreground shadow-sm" : "text-faint hover:text-secondary",
                      "disabled:cursor-not-allowed disabled:opacity-50"
                    )}
                  >
                    {t(`proxy.mode.${m}`)}
                  </button>
                ))}
              </div>
            </div>
          </CardContent>
        </Card>

        {/* 系统代理 */}
        <Card>
          <CardHeader className="flex-row flex-wrap items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-fill">
                <MonitorUp className={cn("h-5 w-5", status?.systemProxyEnabled ? "text-primary" : "text-faint")} strokeWidth={1.8} />
              </div>
              <div>
                <CardTitle className="text-[14px]">{t("proxy.system")}</CardTitle>
                <CardDescription className="mt-1 text-[11px]">{t("proxy.systemHint")}</CardDescription>
              </div>
            </div>
            <Switch
              checked={status?.systemProxyEnabled ?? false}
              onCheckedChange={toggleSystemProxy}
              aria-label={t("proxy.system")}
              disabled={busy || loading || !status || ((!!error || !running) && !status.systemProxyEnabled)}
            />
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2 rounded-lg border border-info/25 bg-info-soft px-3 py-2 text-[11px] text-info">
              <RadioTower className="h-3.5 w-3.5 shrink-0" />
              {t("proxy.sysHint")}
            </div>
          </CardContent>
        </Card>
      </div>

      {/* 订阅配置 */}
      <Card className="mb-6">
        <CardHeader className="flex-row flex-wrap items-center justify-between gap-3">
          <CardTitle className="text-[13px]">{t("proxy.profiles")}</CardTitle>
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => setImportOpen(true)}>
            <Plus className="h-3.5 w-3.5" /> {t("proxy.import")}
          </Button>
        </CardHeader>
        <CardContent>
          {profilesLoading ? (
            <p role="status" className="py-6 text-center text-xs text-faint">{t("common.loading")}</p>
          ) : profilesError ? (
            <p role="status" className="py-6 text-center text-xs text-error">{t("proxy.profilesFailed")}</p>
          ) : profiles.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-xs text-faint">
              {t("proxy.importHint")}
            </p>
          ) : (
            <div className="flex flex-col">
                {profiles.map((p) => (
                  <div key={p.id} className="relative flex flex-wrap items-center gap-2 py-3 px-2 after:absolute after:bottom-0 after:left-3 after:right-3 after:border-b after:border-dashed after:border-border last:after:hidden">
                    <Link2 className={cn("h-3.5 w-3.5 shrink-0", p.active ? "text-running" : "text-faint")} />
                    <div className="min-w-0 flex-1 basis-32">
                      <p className="break-words text-[12.5px] font-medium">{p.name}</p>
                      <p className="truncate font-mono text-[10.5px] text-faint">{subscriptionLabel(p.url)}</p>
                    </div>
                    <div className="ml-auto flex flex-wrap items-center gap-1">
                      {p.active ? (
                        <Badge variant="running"><Check className="h-3 w-3" /> {t(running ? "proxy.inUse" : "proxy.selectedOffline")}</Badge>
                      ) : (
                        <Button size="sm" variant="ghost" disabled={locked} onClick={() => changeProfile(p.id, p.name, "activate")}>
                          {profileAction?.id === p.id && profileAction.kind === "activate" && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                          {t("proxy.enable")}
                        </Button>
                      )}
                      {!p.url.startsWith("builtin:") && <Button size="icon-sm" variant="ghost" disabled={locked}
                        title={t("proxy.updateSub")} aria-label={`${t("proxy.updateSub")} · ${p.name}`}
                        onClick={() => changeProfile(p.id, p.name, "update")}>
                        <RefreshCw className={cn("h-3.5 w-3.5", profileAction?.id === p.id && profileAction.kind === "update" && "animate-spin")} />
                      </Button>}
                      <Button size="icon-sm" variant="ghost" className="text-faint hover:text-error" disabled={locked || p.active}
                        title={p.active ? t("proxy.cannotDeleteActive") : t("common.delete")}
                        aria-label={`${t("common.delete")} · ${p.name}`} onClick={() => setDeleting({ id: p.id, name: p.name })}>
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </div>
                ))}
            </div>
          )}
        </CardContent>
      </Card>

      {/* 节点 */}
      <h2 className="mb-3 text-[15px] font-semibold">{t("proxy.nodes")}</h2>
      {loading || (running && nodesLoading && !nodesError && !error) ? (
        <p role="status" className="py-10 text-center text-xs text-faint">{t("common.loading")}</p>
      ) : error ? (
        <p role="status" className="py-10 text-center text-xs text-error">{t("proxy.nodesFailed")}</p>
      ) : !running ? (
        <p className="rounded-2xl border border-dashed border-border px-4 py-10 text-center text-xs text-faint">
          {t("proxy.nodesHint")}
        </p>
      ) : groups.length === 0 ? (
        <p className="rounded-2xl border border-dashed border-border px-4 py-10 text-center text-xs text-faint">
          {t("proxy.noGroups")}
        </p>
      ) : (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          {groups.map((g) => (
            <ProxyGroupCard key={`${profiles.find((p) => p.active)?.id}:${g.name}`} group={g} onChange={refresh} disabled={busy} />
          ))}
        </div>
      )}

      {/* 实时连接（mihomo /connections，2s 轮询） */}
      {running && <ConnectionsCard />}

      <ImportProfileDialog open={importOpen} onOpenChange={setImportOpen} onDone={refresh} />

      <ConfirmDialog
        open={deleting !== null}
        loading={busy && profileAction?.kind === "delete"}
        onOpenChange={(o) => !o && !actionRef.current && setDeleting(null)}
        title={`${t("confirm.deleteProfile")} · ${deleting?.name ?? ""}`}
        description={t("confirm.deleteProfileDesc")}
        confirmText={t("common.delete")}
        danger
        onConfirm={async () => {
          if (!deleting || actionRef.current) return;
          const target = deleting;
          setProfileAction({ id: target.id, kind: "delete" });
          await perform(async () => {
            await api.proxyDeleteProfile(target.id);
            toast.success(`${t("common.delete")} ${target.name}`);
            setDeleting(null);
          });
        }}
      />
    </div>
  );
}

function ProxyGroupCard({ group, onChange, disabled }: { group: ProxyGroupView; onChange: () => void; disabled: boolean }) {
  const t = useT();
  const [testing, setTesting] = React.useState<string | null>(null);
  const [delays, setDelays] = React.useState<Record<string, number>>({});
  const [batch, setBatch] = React.useState<{ done: number; total: number } | null>(null);
  const [sortByDelay, setSortByDelay] = React.useState(false);
  const [selecting, setSelecting] = React.useState(false);
  const selectionBusy = React.useRef(false);
  const selectable = group.type === "Selector";
  const selectNode = async (name: string) => {
    if (!selectable || disabled || selectionBusy.current || group.now === name) return;
    selectionBusy.current = true;
    setSelecting(true);
    try { await api.proxySelectNode(group.name, name); await onChange(); }
    catch (e) { toastError(e); }
    finally { selectionBusy.current = false; setSelecting(false); }
  };

  const testNode = async (node: string) => {
    setTesting(node);
    try {
      const ms = await api.proxyDelayTest(node);
      setDelays((d) => ({ ...d, [node]: ms }));
    } catch {
      setDelays((d) => ({ ...d, [node]: -1 }));
    } finally {
      setTesting(null);
    }
  };

  /** 并发测速全部节点（并发 6）：串行的话几十个节点要等几分钟 */
  const testAll = async () => {
    if (batch) return;
    const nodes = group.nodes.map((n) => n.name);
    if (nodes.length === 0) return;
    setBatch({ done: 0, total: nodes.length });
    let idx = 0;
    const worker = async () => {
      while (idx < nodes.length) {
        const n = nodes[idx++];
        try {
          const ms = await api.proxyDelayTest(n);
          setDelays((d) => ({ ...d, [n]: ms }));
        } catch {
          setDelays((d) => ({ ...d, [n]: -1 }));
        }
        setBatch((b) => (b ? { ...b, done: b.done + 1 } : b));
      }
    };
    await Promise.all(Array.from({ length: Math.min(6, nodes.length) }, worker));
    setBatch(null);
  };

  const delayOf = (n: ProxyGroupView["nodes"][number]): number | null => {
    if (delays[n.name] != null) return delays[n.name];
    return n.history?.length ? n.history[n.history.length - 1] : null;
  };
  const alive = (v: number | null) => v != null && v >= 0;

  /** 测过的节点里最快的（排除超时） */
  const fastest = React.useMemo(() => {
    let best: string | null = null;
    let bestMs = Infinity;
    for (const n of group.nodes) {
      const d = delays[n.name];
      if (d != null && d >= 0 && d < bestMs) {
        bestMs = d;
        best = n.name;
      }
    }
    return best;
  }, [group.nodes, delays]);

  const shown = React.useMemo(() => {
    if (!sortByDelay) return group.nodes;
    return [...group.nodes].sort((a, b) => {
      const da = delayOf(a);
      const db = delayOf(b);
      if (!alive(da) && !alive(db)) return 0;
      if (!alive(da)) return 1;
      if (!alive(db)) return -1;
      return (da as number) - (db as number);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sortByDelay, group.nodes, delays]);

  const switchFastest = async () => {
    if (!fastest || fastest === group.now) return;
    await selectNode(fastest);
  };

  return (
    <Card className="min-w-0 p-3 sm:p-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <p className="break-all text-[13px] font-medium">{group.name}</p>
          <p className="break-all text-[10.5px] text-faint">
            {group.type} · {t("proxy.current")}: <span className="text-primary">{group.now}</span>
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                size="icon-sm"
                variant="ghost"
                className={cn("text-faint hover:text-secondary", sortByDelay && "bg-primary-soft text-primary hover:text-primary")}
                onClick={() => setSortByDelay((v) => !v)}
                aria-label={t("proxy.sortByDelay")}
                aria-pressed={sortByDelay}
              >
                <ArrowDownUp className="h-3.5 w-3.5" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t("proxy.sortByDelay")}</TooltipContent>
          </Tooltip>
          {selectable && fastest && fastest !== group.now && (
            <Button size="sm" variant="secondary" disabled={disabled || selecting} onClick={switchFastest}>
              <Zap className="h-3 w-3" /> {t("proxy.useFastest")}
            </Button>
          )}
          <Button size="sm" variant="secondary" onClick={testAll} disabled={disabled || !!batch || !!testing}>
            <Gauge className={cn("h-3 w-3", batch && "animate-pulse")} />
            {batch ? `${batch.done}/${batch.total}` : t("proxy.testAll")}
          </Button>
        </div>
      </div>
      <div className="flex flex-col gap-1">
        {shown.map((n) => {
          const active = group.now === n.name;
          const delay = delays[n.name];
          return (
            <div
              key={n.name}
              className={cn(
                "group flex min-w-0 flex-wrap items-center gap-2 rounded-lg border px-2 py-2 transition-all",
                active ? "border-primary/50 bg-primary-soft" : "border-transparent hover:border-border hover:bg-card-2/50"
              )}
            >
              <button type="button" className="flex min-w-0 flex-1 basis-28 items-center gap-2 text-left disabled:cursor-default"
                disabled={!selectable || selecting || disabled} aria-pressed={active}
                aria-label={`${group.name} · ${n.name}`} onClick={() => void selectNode(n.name)}>
              {active ? (
                <Check className="h-3.5 w-3.5 shrink-0 text-primary" strokeWidth={2.5} />
              ) : (
                <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-faint/40" />
              )}
              <span className="min-w-0 flex-1 break-words text-[12.5px]">{n.name}<span className="block text-[10px] text-faint">{n.type}</span></span>
              </button>
              {testing === n.name ? (
                <Badge variant="info">{t("proxy.testing")}</Badge>
              ) : delay != null ? (
                <DelayBadge ms={delay} />
              ) : n.history?.length ? (
                <DelayBadge ms={n.history[n.history.length - 1]} />
              ) : n.alive === false ? (
                <Badge variant="error">
                  <SignalZero className="h-3 w-3" /> {t("proxy.unavailable")}
                </Badge>
              ) : null}
              <Button
                size="icon-sm"
                variant="ghost"
                className="shrink-0"
                disabled={disabled || !!testing || !!batch}
                aria-label={`${t("proxy.testing")} · ${n.name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  testNode(n.name);
                }}
              >
                <Signal className="h-3 w-3" />
              </Button>
            </div>
          );
        })}
      </div>
    </Card>
  );
}

function DelayBadge({ ms }: { ms: number }) {
  const t = useT();
  if (ms < 0) return <Badge variant="error">{t("proxy.timeout")}</Badge>;
  if (ms < 150) return <Badge variant="running">{ms}ms</Badge>;
  if (ms < 400) return <Badge variant="warn">{ms}ms</Badge>;
  return <Badge variant="error">{ms}ms</Badge>;
}

/** 实时连接：展开后 2s 轮询 /connections，展示累计流量、实时速率与活跃连接明细 */
function ConnectionsCard() {
  const t = useT();
  const [open, setOpen] = React.useState(false);
  const connections = useQuery({ queryKey: ["proxy-connections"], queryFn: api.proxyConnections,
    enabled: open, refetchInterval: open ? 2000 : false, retry: false });
  const data = connections.data;
  const info = data ? { downloadTotal: data.downloadTotal ?? 0, uploadTotal: data.uploadTotal ?? 0, conns: data.connections ?? [] } : null;
  const [speed, setSpeed] = React.useState({ dl: 0, ul: 0 });
  const prev = React.useRef<{ dl: number; ul: number; at: number } | null>(null);
  React.useEffect(() => {
    if (!open || connections.error) { prev.current = null; setSpeed({ dl: 0, ul: 0 }); return; }
    if (!data) return;
    const dl = data.downloadTotal ?? 0;
    const ul = data.uploadTotal ?? 0;
    const at = connections.dataUpdatedAt;
    if (prev.current && at > prev.current.at) {
      const seconds = (at - prev.current.at) / 1000;
      setSpeed({ dl: Math.max(0, (dl - prev.current.dl) / seconds), ul: Math.max(0, (ul - prev.current.ul) / seconds) });
    }
    prev.current = { dl, ul, at };
  }, [open, data, connections.dataUpdatedAt, connections.error]);

  const conns = [...(info?.conns ?? [])].sort((a, b) => b.download - a.download).slice(0, 30);

  return (
    <Card className="mt-6">
      <CardHeader className="flex-row flex-wrap items-center justify-between gap-3 py-4">
        <div className="flex min-w-0 items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-fill">
            <Activity className="h-4 w-4 text-primary" strokeWidth={1.8} />
          </div>
          <div>
            <CardTitle className="text-[13px]">{t("proxy.connections")}</CardTitle>
            <CardDescription className="mt-0.5 text-[11px]">
              {info ? t("proxy.connCount").replace("{n}", String(info.conns.length)) : open ? t("common.loading") : t("proxy.connExpand")}
            </CardDescription>
          </div>
        </div>
        <Button size="sm" variant="ghost" onClick={() => setOpen((v) => !v)}>
          {open ? t("proxy.connCollapse") : t("proxy.connExpand")}
          <ChevronDown className={cn("h-3.5 w-3.5 transition-transform", open && "rotate-180")} />
        </Button>
      </CardHeader>
      {open && connections.error && <div role="alert" className="mx-4 mb-3 flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
        <span className="min-w-0 flex-1">{t("proxy.connectionsFailed")}</span>
        <Button size="sm" variant="ghost" disabled={connections.isFetching} onClick={() => void connections.refetch()}>{t("proxy.retry")}</Button>
      </div>}
      {open && connections.isPending && <p role="status" className="px-4 pb-4 text-xs text-faint">{t("common.loading")}</p>}
      {open && info && (
        <CardContent className="pt-0">
          <div className="mb-3 grid grid-cols-2 gap-2 md:grid-cols-4">
            <Stat label={t("proxy.downTotal")} value={fmtBytes(info.downloadTotal)} />
            <Stat label={t("proxy.upTotal")} value={fmtBytes(info.uploadTotal)} />
            <Stat label={t("proxy.downSpeed")} value={connections.error ? "—" : `${fmtBytes(speed.dl)}/s`} highlight />
            <Stat label={t("proxy.upSpeed")} value={connections.error ? "—" : `${fmtBytes(speed.ul)}/s`} highlight />
          </div>
          {conns.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-xs text-faint">
              {t("proxy.connNone")}
            </p>
          ) : (
            <div className="max-h-80 overflow-auto rounded-lg border border-border">
              <table className="w-full min-w-[480px] text-left text-[11.5px]">
                <thead className="sticky top-0 bg-card-2/60 text-[10px] uppercase tracking-wide text-faint">
                  <tr>
                    <th className="px-3 py-1.5 font-medium">{t("proxy.connHost")}</th>
                    <th className="px-3 py-1.5 font-medium">{t("proxy.connChain")}</th>
                    <th className="px-3 py-1.5 text-right font-medium">{t("proxy.connDown")}</th>
                    <th className="px-3 py-1.5 text-right font-medium">{t("proxy.connUp")}</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border/60">
                  {conns.map((c) => {
                    const host = c.metadata?.host || c.metadata?.destinationIP || "—";
                    const chain = c.chains?.length ? c.chains[0] : "—";
                    return (
                      <tr key={c.id} className="hover:bg-card-2/30">
                        <td className="max-w-0 truncate px-3 py-1.5 font-mono text-[11px]">
                          {host}
                          {c.metadata?.destinationPort ? `:${c.metadata.destinationPort}` : ""}
                          {c.metadata?.type && (
                            <span className="ml-1.5 rounded bg-card-2 px-1 text-[9.5px] text-faint">
                              {c.metadata.type}
                            </span>
                          )}
                        </td>
                        <td className="max-w-0 truncate px-3 py-1.5 text-faint">{chain}</td>
                        <td className="px-3 py-1.5 text-right tabular text-secondary">{fmtBytes(c.download)}</td>
                        <td className="px-3 py-1.5 text-right tabular text-secondary">{fmtBytes(c.upload)}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      )}
    </Card>
  );
}

function Stat({ label, value, highlight }: { label: string; value: string; highlight?: boolean }) {
  return (
    <div className="rounded-lg border border-border px-3 py-2">
      <p className="text-[10px] text-faint">{label}</p>
      <p className={cn("mt-0.5 tabular text-[13px] font-medium", highlight && "text-primary")}>{value}</p>
    </div>
  );
}

function ImportProfileDialog({
  open,
  onOpenChange,
  onDone,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onDone: () => void;
}) {
  const t = useT();
  const [name, setName] = React.useState("");
  const [url, setUrl] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const pending = React.useRef(false);
  const [error, setError] = React.useState<string | null>(null);
  const submit = async () => {
    if (pending.current) return;
    let parsed: URL;
    try {
      parsed = new URL(url.trim());
      if (!["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password) throw new Error();
    } catch { setError(t("proxy.invalidUrl")); return; }
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      await api.proxyImport(name.trim() || t("proxy.mySubs"), parsed.href);
      toast.success(t(isTauri ? "proxy.subImported" : "proxy.previewImported"));
      onOpenChange(false);
      setName("");
      setUrl("");
      onDone();
    } catch (e) {
      const failure = normalizeError(e);
      setError(`${failure.message}${failure.hint ? ` · ${failure.hint}` : ""}`);
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(next) => { if (!pending.current) onOpenChange(next); }}>
      <DialogContent className="max-w-md" hideClose={busy}>
        <DialogHeader>
          <DialogTitle>{t("proxy.importTitle")}</DialogTitle>
          <DialogDescription>
            {t("proxy.privacyHint")}
          </DialogDescription>
        </DialogHeader>
        {!isTauri && <p className="rounded-lg bg-info-soft p-3 text-xs text-info">{t("proxy.previewHint")}</p>}
        <div className="flex flex-col gap-3">
          <label className="space-y-1.5 text-xs text-muted">{t("proxy.nameLabel")}
            <Input value={name} disabled={busy} maxLength={128} onChange={(e) => setName(e.target.value)} placeholder={t("proxy.namePlaceholder")} />
          </label>
          <label className="space-y-1.5 text-xs text-muted">{t("proxy.urlLabel")}
            <Input type="url" value={url} disabled={busy} maxLength={8192} autoComplete="off" spellCheck={false}
              onChange={(e) => { setUrl(e.target.value); setError(null); }} placeholder="https://example.com/sub?token=…" className="font-mono text-[12px]" />
          </label>
          {error && <p role="alert" className="break-words text-xs text-error">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="ghost" disabled={busy} onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={!url.trim() || busy}>{busy ? t("proxy.importing") : t("proxy.import")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
