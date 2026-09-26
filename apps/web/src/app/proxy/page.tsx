"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
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
} from "lucide-react";
import type { ProxyGroupView } from "@nsb/schema";
import { cn, fmtBytes, fmtSpeed } from "@/lib/utils";
import { useUI, useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import { normalizeError } from "@/lib/backend";
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

/* mihomo 状态轮询 */
function useProxyState() {
  const [status, setStatus] = React.useState<{
    running: boolean;
    mixedPort: number;
    controllerPort: number;
    mode: "rule" | "global" | "direct";
    systemProxyEnabled: boolean;
    version?: string;
  } | null>(null);
  const [groups, setGroups] = React.useState<ProxyGroupView[]>([]);
  const [profiles, setProfiles] = React.useState<
    { id: string; name: string; url: string; active: boolean }[]
  >([]);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState<string | null>(null);
  const firstLoad = React.useRef(true);

  const refresh = React.useCallback(async () => {
    if (firstLoad.current) setLoading(true);
    try {
      const s = await api.proxyStatus();
      setStatus(s);
      if (s.running) {
        const [g, p] = await Promise.all([api.proxyNodes(), api.proxyProfiles()]);
        setGroups(g);
        setProfiles(p);
      } else {
        const p = await api.proxyProfiles().catch(() => []);
        setProfiles(p);
        setGroups([]);
      }
      setError(null);
    } catch (e) {
      // 保留上一次有效状态，避免一次短暂错误把真实配置显示成空白。
      setError(normalizeError(e).message);
    } finally {
      firstLoad.current = false;
      setLoading(false);
    }
  }, []);

  React.useEffect(() => {
    refresh();
    const id = setInterval(refresh, 3000);
    return () => clearInterval(id);
  }, [refresh]);

  return { status, groups, profiles, refresh, loading, error };
}

export default function ProxyPage() {
  const t = useT();
  const { status, groups, profiles, refresh, loading, error } = useProxyState();
  const [importOpen, setImportOpen] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [deleting, setDeleting] = React.useState<{ id: string; name: string } | null>(null);
  const [updatingId, setUpdatingId] = React.useState<string | null>(null);

  const running = status?.running ?? false;

  const toggleCore = async (next: boolean) => {
    setBusy(true);
    try {
      if (next) await api.proxyStart();
      else {
        if (status?.systemProxyEnabled) await api.proxySetSystem(false).catch(() => undefined);
        await api.proxyStop();
      }
      toast.success(next ? t("proxy.coreStarted") : t("proxy.coreStopped"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
      refresh();
    }
  };

  const toggleSystemProxy = async (next: boolean) => {
    try {
      if (next && !running) {
        toast.error(t("proxy.sysPrecondition"));
        return;
      }
      await api.proxySetSystem(next);
      toast.success(next ? `${t("proxy.sysOn")} → 127.0.0.1:${status?.mixedPort}` : t("proxy.sysOff"));
    } catch (e) {
      toastError(e);
    } finally {
      refresh();
    }
  };

  const setMode = async (mode: "rule" | "global" | "direct") => {
    try {
      await api.proxySetMode(mode);
      refresh();
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <div className="pb-8">
      <PageHeader title={t("proxy.title")} subtitle={t("proxy.subtitle")} />

      {error && (
        <div role="alert" className="mb-4 flex flex-wrap items-center gap-3 rounded-xl border border-error/25 bg-error-soft/40 p-3 text-xs [overflow-wrap:anywhere]">
          <span className="min-w-0 flex-1 text-error">{t("proxy.readFailed")}：{error}</span>
          <Button size="sm" variant="secondary" disabled={loading} onClick={() => void refresh()}>
            <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} /> {t("proxy.retry")}
          </Button>
        </div>
      )}

      <div className="mb-6 grid grid-cols-1 gap-4 md:grid-cols-2">
        {/* 内核控制 */}
        <Card className={cn(running && "breath border-running/25")}>
          <CardHeader className="flex-row items-center justify-between">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-fill">
                <Waypoints className={cn("h-5 w-5", running ? "text-running" : "text-faint")} strokeWidth={1.8} />
              </div>
              <div>
                <CardTitle className="flex items-center gap-2 text-[14px]">
                  mihomo <StatusLight state={running ? "running" : "stopped"} size={7} />
                </CardTitle>
                <CardDescription className="mt-1 font-mono text-[11px]">
                  {status ? `${t("proxy.mixedPort")} ${status.mixedPort} · API ${status.controllerPort}${status.version ? ` · ${status.version}` : ""}` : t("common.loading")}
                </CardDescription>
              </div>
            </div>
            <Switch checked={running} onCheckedChange={toggleCore} disabled={busy || loading || !status} />
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <span className="text-[11.5px] text-muted">{t("proxy.mode")}</span>
              <div className="flex gap-1 rounded-lg bg-card-2/60 p-1">
                {(["rule", "global", "direct"] as const).map((m) => (
                  <button
                    key={m}
                    onClick={() => setMode(m)}
                    disabled={loading || !status}
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
          <CardHeader className="flex-row items-center justify-between">
            <div className="flex items-center gap-3">
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
              disabled={loading || !status}
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
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle className="text-[13px]">{t("proxy.profiles")}</CardTitle>
          <Button size="sm" variant="secondary" onClick={() => setImportOpen(true)}>
            <Plus className="h-3.5 w-3.5" /> {t("proxy.import")}
          </Button>
        </CardHeader>
        <CardContent>
          {profiles.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-xs text-faint">
              {t("proxy.importHint")}
            </p>
          ) : (
            <div className="flex flex-col divide-y divide-border">
              <AnimatePresence>
                {profiles.map((p) => (
                  <motion.div key={p.id} layout initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="flex items-center gap-3 py-2.5">
                    <Link2 className={cn("h-3.5 w-3.5 shrink-0", p.active ? "text-running" : "text-faint")} />
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-[12.5px] font-medium">{p.name}</p>
                      <p className="truncate font-mono text-[10.5px] text-faint">{p.url}</p>
                    </div>
                    {p.active ? (
                      <Badge variant="running">
                        <Check className="h-3 w-3" /> {t("proxy.inUse")}
                      </Badge>
                    ) : (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={async () => {
                          try {
                            await api.proxyActivateProfile(p.id);
                            toast.success(`${t("proxy.switchedP1")} ${p.name}`);
                            refresh();
                          } catch (e) {
                            toastError(e);
                          }
                        }}
                      >
                        {t("proxy.enable")}
                      </Button>
                    )}
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          className="text-faint hover:text-secondary"
                          disabled={updatingId === p.id}
                          onClick={async () => {
                            setUpdatingId(p.id);
                            try {
                              await api.proxyUpdateProfile(p.id);
                              toast.success(`${p.name} · ${t("proxy.subUpdated")}`);
                              refresh();
                            } catch (e) {
                              toastError(e, t("proxy.importFailed"));
                            } finally {
                              setUpdatingId(null);
                            }
                          }}
                        >
                          <RefreshCw className={cn("h-3.5 w-3.5", updatingId === p.id && "animate-spin")} />
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>{t("proxy.updateSub")}</TooltipContent>
                    </Tooltip>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          className="text-faint hover:text-error"
                          disabled={p.active}
                          title={p.active ? t("proxy.cannotDeleteActive") : t("common.delete")}
                          aria-label={t("common.delete")}
                          onClick={() => setDeleting({ id: p.id, name: p.name })}
                        >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
          )}
        </CardContent>
      </Card>

      {/* 节点 */}
      <h2 className="mb-3 text-[15px] font-semibold">{t("proxy.nodes")}</h2>
      {!running ? (
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
            <ProxyGroupCard key={g.name} group={g} onChange={refresh} />
          ))}
        </div>
      )}

      {/* 实时连接（mihomo /connections，2s 轮询） */}
      {running && <ConnectionsCard />}

      <ImportProfileDialog open={importOpen} onOpenChange={setImportOpen} onDone={refresh} />

      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={`${t("confirm.deleteProfile")} · ${deleting?.name ?? ""}`}
        description={t("confirm.deleteProfileDesc")}
        confirmText={t("common.delete")}
        danger
        onConfirm={async () => {
          if (!deleting) return;
          try {
            await api.proxyDeleteProfile(deleting.id);
            toast.success(`${t("common.delete")} ${deleting.name}`);
            refresh();
          } catch (e) {
            toastError(e);
          } finally {
            setDeleting(null);
          }
        }}
      />
    </div>
  );
}

function ProxyGroupCard({ group, onChange }: { group: ProxyGroupView; onChange: () => void }) {
  const t = useT();
  const [testing, setTesting] = React.useState<string | null>(null);
  const [delays, setDelays] = React.useState<Record<string, number>>({});
  const [batch, setBatch] = React.useState<{ done: number; total: number } | null>(null);
  const [sortByDelay, setSortByDelay] = React.useState(false);

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
    const nodes = group.nodes.filter((n) => n.alive !== false).map((n) => n.name);
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
    try {
      await api.proxySelectNode(group.name, fastest);
      toast.success(`${group.name} → ${fastest} (${delays[fastest]}ms)`);
      onChange();
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <Card className="p-4">
      <div className="mb-3 flex items-center justify-between gap-2">
        <div className="min-w-0">
          <p className="text-[13px] font-medium">{group.name}</p>
          <p className="text-[10.5px] text-faint">
            {group.type} · {t("proxy.current")}: <span className="text-primary">{group.now}</span>
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                size="icon-sm"
                variant="ghost"
                className={cn("text-faint hover:text-secondary", sortByDelay && "bg-primary-soft text-primary hover:text-primary")}
                onClick={() => setSortByDelay((v) => !v)}
              >
                <ArrowDownUp className="h-3.5 w-3.5" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t("proxy.sortByDelay")}</TooltipContent>
          </Tooltip>
          {fastest && fastest !== group.now && (
            <Button size="sm" variant="secondary" onClick={switchFastest}>
              <Zap className="h-3 w-3" /> {t("proxy.useFastest")}
            </Button>
          )}
          <Button size="sm" variant="secondary" onClick={testAll} disabled={!!batch}>
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
                "group flex cursor-pointer items-center gap-2.5 rounded-lg border px-3 py-2 transition-all",
                active ? "border-primary/50 bg-primary-soft" : "border-transparent hover:border-border hover:bg-card-2/50"
              )}
              onClick={async () => {
                try {
                  await api.proxySelectNode(group.name, n.name);
                  toast.success(`${group.name} → ${n.name}`);
                  onChange();
                } catch (e) {
                  toastError(e);
                }
              }}
            >
              {active ? (
                <Check className="h-3.5 w-3.5 shrink-0 text-primary" strokeWidth={2.5} />
              ) : (
                <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-faint/40" />
              )}
              <span className="flex-1 truncate text-[12.5px]">{n.name}</span>
              <span className="text-[10px] text-faint">{n.type}</span>
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
                className="opacity-0 group-hover:opacity-100"
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
  const [info, setInfo] = React.useState<{
    downloadTotal: number;
    uploadTotal: number;
    conns: api.ProxyConnection[];
  } | null>(null);
  const [speed, setSpeed] = React.useState({ dl: 0, ul: 0 });
  const prev = React.useRef<{ dl: number; ul: number; at: number } | null>(null);

  React.useEffect(() => {
    if (!open) return;
    let stop = false;
    const poll = async () => {
      try {
        const d = await api.proxyConnections();
        if (stop) return;
        const dl = d.downloadTotal ?? 0;
        const ul = d.uploadTotal ?? 0;
        const now = Date.now();
        if (prev.current) {
          const dt = (now - prev.current.at) / 1000;
          if (dt > 0.5) {
            setSpeed({
              dl: Math.max(0, (dl - prev.current.dl) / dt),
              ul: Math.max(0, (ul - prev.current.ul) / dt),
            });
          }
        }
        prev.current = { dl, ul, at: now };
        setInfo({ downloadTotal: dl, uploadTotal: ul, conns: d.connections ?? [] });
      } catch {
        /* 内核没起或接口不可用：静默等下一轮 */
      }
    };
    void poll();
    const id = setInterval(poll, 2000);
    return () => {
      stop = true;
      clearInterval(id);
      prev.current = null;
    };
  }, [open]);

  const conns = [...(info?.conns ?? [])].sort((a, b) => b.download - a.download).slice(0, 30);

  return (
    <Card className="mt-6">
      <CardHeader className="flex-row items-center justify-between py-4">
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-fill">
            <Activity className="h-4 w-4 text-primary" strokeWidth={1.8} />
          </div>
          <div>
            <CardTitle className="text-[13px]">{t("proxy.connections")}</CardTitle>
            <CardDescription className="mt-0.5 text-[11px]">
              {info ? t("proxy.connCount").replace("{n}", String(info.conns.length)) : t("common.loading")}
            </CardDescription>
          </div>
        </div>
        <Button size="sm" variant="ghost" onClick={() => setOpen((v) => !v)}>
          {open ? t("proxy.connCollapse") : t("proxy.connExpand")}
          <ChevronDown className={cn("h-3.5 w-3.5 transition-transform", open && "rotate-180")} />
        </Button>
      </CardHeader>
      {open && info && (
        <CardContent className="pt-0">
          <div className="mb-3 grid grid-cols-2 gap-2 md:grid-cols-4">
            <Stat label={t("proxy.downTotal")} value={fmtBytes(info.downloadTotal)} />
            <Stat label={t("proxy.upTotal")} value={fmtBytes(info.uploadTotal)} />
            <Stat label={t("proxy.downSpeed")} value={`${fmtBytes(speed.dl)}/s`} highlight />
            <Stat label={t("proxy.upSpeed")} value={`${fmtBytes(speed.ul)}/s`} highlight />
          </div>
          {conns.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-xs text-faint">
              {t("proxy.connNone")}
            </p>
          ) : (
            <div className="max-h-80 overflow-y-auto rounded-lg border border-border">
              <table className="w-full text-left text-[11.5px]">
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
  const submit = async () => {
    setBusy(true);
    try {
      await api.proxyImport(name || t("proxy.mySubs"), url);
      toast.success(t("proxy.subImported"));
      onOpenChange(false);
      setName("");
      setUrl("");
      onDone();
    } catch (e) {
      toastError(e, t("proxy.importFailed"));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t("proxy.importTitle")}</DialogTitle>
          <DialogDescription>
            {t("proxy.privacyHint")}
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("proxy.namePlaceholder")} />
          <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://example.com/sub?token=…" className="font-mono text-[12px]" />
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={!url || busy}>{busy ? t("proxy.importing") : t("proxy.import")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
