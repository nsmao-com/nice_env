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
} from "lucide-react";
import type { ProxyGroupView } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useUI, useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { StatusLight } from "@/components/shared/status-light";
import { ConfirmDialog } from "@/components/shared/misc";
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

  const refresh = React.useCallback(async () => {
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
    } catch {
      /* ignore */
    }
  }, []);

  React.useEffect(() => {
    refresh();
    const id = setInterval(refresh, 3000);
    return () => clearInterval(id);
  }, [refresh]);

  return { status, groups, profiles, refresh };
}

export default function ProxyPage() {
  const t = useT();
  const { status, groups, profiles, refresh } = useProxyState();
  const [importOpen, setImportOpen] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [deleting, setDeleting] = React.useState<{ id: string; name: string } | null>(null);

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

      <div className="mb-6 grid grid-cols-1 gap-4 md:grid-cols-2">
        {/* 内核控制 */}
        <Card className={cn(running && "breath border-running/25")}>
          <CardHeader className="flex-row items-center justify-between">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl border border-border bg-card-2/60">
                <Waypoints className={cn("h-5 w-5", running ? "text-running" : "text-faint")} strokeWidth={1.8} />
              </div>
              <div>
                <CardTitle className="flex items-center gap-2 text-[14px]">
                  mihomo <StatusLight state={running ? "running" : "stopped"} size={7} />
                </CardTitle>
                <CardDescription className="mt-1 font-mono text-[11px]">
                  {status ? `${t("proxy.mixedPort")} ${status.mixedPort} · API ${status.controllerPort}${status.version ? ` · ${status.version}` : ""}` : "加载中…"}
                </CardDescription>
              </div>
            </div>
            <Switch checked={running} onCheckedChange={toggleCore} disabled={busy} />
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <span className="text-[11.5px] text-muted">{t("proxy.mode")}</span>
              <div className="flex gap-1 rounded-lg bg-card-2/60 p-1">
                {(["rule", "global", "direct"] as const).map((m) => (
                  <button
                    key={m}
                    onClick={() => setMode(m)}
                    className={cn(
                      "rounded-md px-2.5 py-1 text-[11.5px] font-medium transition-all",
                      status?.mode === m ? "bg-surface text-foreground shadow-sm" : "text-faint hover:text-secondary"
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
              <div className="flex h-10 w-10 items-center justify-center rounded-xl border border-border bg-card-2/60">
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
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      className="text-faint hover:text-error"
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

  return (
    <Card className="p-4">
      <div className="mb-3 flex items-center justify-between">
        <div>
          <p className="text-[13px] font-medium">{group.name}</p>
          <p className="text-[10.5px] text-faint">
            {group.type} · {t("proxy.current")}: <span className="text-primary">{group.now}</span>
          </p>
        </div>
        <Button
          size="sm"
          variant="secondary"
          onClick={async () => {
            for (const n of group.nodes) {
              if (n.alive !== false) await testNode(n.name);
            }
          }}
        >
          <Gauge className="h-3 w-3" /> {t("proxy.testAll")}
        </Button>
      </div>
      <div className="flex flex-col gap-1">
        {group.nodes.map((n) => {
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
