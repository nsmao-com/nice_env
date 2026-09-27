"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import { Bell, Globe, Loader2, Plus, RefreshCw, Trash2, X } from "lucide-react";
import type { CertMonitor } from "@nsb/schema";
import { useT } from "@/lib/store";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog } from "@/components/shared/misc";

/**
 * 网站证书监控（certd 的「站点证书监控」）：
 * 盯任意站点 / 设备（路由器、NAS…）的证书到期时间。TLS 握手读对端证书链，
 * 不校验（自签/过期也要能看到），每小时随调度自动刷新，也可手动「立即检查」。
 */
/** 监控告警推送设置（读写全局 settings monitorNotifyKind/Url） */
function MonitorNotifySetting() {
  const t = useT();
  const qc = useQueryClient();
  const query = useQuery({ queryKey: ["monitor-notifications"], queryFn: api.certMonitorNotificationGet });
  const [kind, setKind] = React.useState("none");
  const [url, setUrl] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [open, setOpen] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);

  const save = async () => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try {
      const settings = await api.certMonitorNotificationSave({ kind, url: url.trim() });
      qc.setQueryData(["monitor-notifications"], settings);
      toast.success(t("monitor.notifySaved"));
      setOpen(false);
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      busyRef.current = false; setBusy(false);
    }
  };

  if (!open) {
    if (query.isPending) return <p className="text-xs text-muted">{t("common.loading")}</p>;
    if (query.error) return <div role="alert" className="space-y-2 text-xs text-error">
      <p>{t("monitor.notifyReadFailed")}</p><Button size="sm" variant="secondary" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("bulk.retry")}</Button>
    </div>;
    const configured = query.data?.kind && query.data.kind !== "none" && query.data.url.trim();
    return (
      <button
        type="button"
        onClick={() => { setKind(query.data?.kind || "none"); setUrl(query.data?.url || ""); setError(null); setOpen(true); }}
        className="flex items-center gap-1.5 self-start rounded-md border border-border bg-card-2/40 px-2 py-1 text-[11px] text-muted transition-colors hover:border-border-strong hover:text-foreground"
      >
        <Bell className="h-3 w-3" />
        {configured ? t("monitor.notifyOn") : t("monitor.notifyOff")}
      </button>
    );
  }
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border p-2.5">
      <div className="flex items-center justify-between">
        <span className="text-[11.5px] font-medium text-secondary">{t("monitor.notifyTitle")}</span>
        <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} disabled={busy} onClick={() => setOpen(false)}><X className="h-3.5 w-3.5" /></Button>
      </div>
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
        <Select value={kind} onValueChange={setKind} disabled={busy}>
          <SelectTrigger aria-label={t("monitor.notifyTitle")} className="h-8 text-[12px]"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="none">{t("certauto.notify.none")}</SelectItem>
            <SelectItem value="generic">{t("certauto.notify.generic")}</SelectItem>
            <SelectItem value="dingtalk">{t("certauto.notify.dingtalk")}</SelectItem>
            <SelectItem value="wecom">{t("certauto.notify.wecom")}</SelectItem>
            <SelectItem value="feishu">{t("certauto.notify.feishu")}</SelectItem>
          </SelectContent>
        </Select>
        {kind !== "none" && <Input
          value={url}
          type="password"
          autoComplete="off"
          aria-label={t("certauto.notifyUrl")}
          disabled={busy}
          onChange={(e) => setUrl(e.target.value)}
          placeholder={t("certauto.notifyUrl")}
          className="font-mono text-[12px]"
        />}
      </div>
      {error && <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{error.message}</p>}
      <div className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant="secondary" disabled={busy || (kind !== "none" && !url.trim())} onClick={save}>
          {busy ? <Loader2 className="h-3 w-3 animate-spin" /> : null} {t("common.save")}
        </Button>
        <p className="text-[10.5px] text-faint">{t("monitor.notifyHint")}</p>
      </div>
    </div>
  );
}

export function CertMonitorCard() {
  const t = useT();
  const qc = useQueryClient();
  const invalidate = React.useCallback(() => qc.invalidateQueries({ queryKey: ["certmonitors"] }), [qc]);
  const query = useQuery({
    queryKey: ["certmonitors"],
    queryFn: api.certMonitorList,
    refetchInterval: 60_000,
  });
  const monitors = query.data ?? [];

  const [host, setHost] = React.useState("");
  const [adding, setAdding] = React.useState(false);
  const addingRef = React.useRef(false);
  const checkingRef = React.useRef(new Set<string>());
  const [checking, setChecking] = React.useState(new Set<string>());
  const [removing, setRemoving] = React.useState<CertMonitor | null>(null);
  const [deleting, setDeleting] = React.useState(false);
  const deletingRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [deleteError, setDeleteError] = React.useState<AppErrorShape | null>(null);

  const add = async () => {
    if (!host.trim() || addingRef.current) return;
    addingRef.current = true; setAdding(true); setError(null);
    let saved = false;
    try {
      const m = await api.certMonitorAdd({
        id: "", name: "", host: host.trim(), port: 443,
        state: "idle", issuer: "", lastError: "",
        expiresAt: null, lastChecked: null,
        createdAt: 0, updatedAt: 0,
      });
      saved = true; setHost("");
      await invalidate();
      // 加完立刻查一次，让用户马上看到结果
      await api.certMonitorCheck(m.id);
    } catch (e) {
      const normalized = normalizeError(e);
      setError(saved ? { ...normalized, message: `${t("monitor.addedCheckFailed")} ${normalized.message}` } : normalized);
    } finally {
      await invalidate(); addingRef.current = false; setAdding(false);
    }
  };

  const checkNow = async (m: CertMonitor) => {
    if (checkingRef.current.has(m.id)) return;
    checkingRef.current.add(m.id); setChecking(new Set(checkingRef.current)); setError(null);
    try {
      await api.certMonitorCheck(m.id);
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      await invalidate(); checkingRef.current.delete(m.id); setChecking(new Set(checkingRef.current));
    }
  };

  const daysLeft = (m: CertMonitor) =>
    m.expiresAt != null ? Math.floor((m.expiresAt - Date.now()) / 86400_000) : null;

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-2">
          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-border bg-card-2/60">
            <Globe className="h-4 w-4 text-primary" strokeWidth={1.8} />
          </div>
          <CardTitle className="min-w-0 flex-1 text-[13px] leading-snug">{t("monitor.title")}</CardTitle>
          <Button size="icon-sm" variant="ghost" aria-label={t("monitor.refreshList")} disabled={query.isFetching} onClick={() => void query.refetch()}>
            <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin")} />
          </Button>
        </div>
        <CardDescription className="text-[11px]">{t("monitor.hint")}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {/* 添加行 */}
        <div className="flex flex-wrap gap-2">
          <Input
            value={host}
            onChange={(e) => setHost(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) { e.preventDefault(); void add(); } }}
            aria-label={t("monitor.address")}
            disabled={adding}
            placeholder={t("monitor.placeholder")}
            className="min-w-0 flex-1 basis-40 font-mono text-[12px]"
          />
          <Button variant="secondary" disabled={adding || !host.trim()} onClick={add}>
            {adding ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Plus className="h-3.5 w-3.5" />}
            {t("monitor.add")}
          </Button>
        </div>

        {error && <p role="alert" className="rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">{error.message}</p>}
        {query.error && <div role="alert" className="space-y-2 text-xs text-error [overflow-wrap:anywhere]">
          <p>{t("monitor.readFailed")} {normalizeError(query.error).message}</p>
          <Button size="sm" variant="secondary" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("bulk.retry")}</Button>
        </div>}
        {query.isPending ? <p className="py-4 text-xs text-muted">{t("common.loading")}</p> : !query.error && monitors.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-[11.5px] text-faint">
            {t("monitor.empty")}
          </p>
        ) : monitors.length > 0 ? (
          <div className="flex min-w-0 flex-col rounded-lg border border-border bg-card-2/20">
            <AnimatePresence initial={false}>
              {monitors.map((m, index) => {
                const days = daysLeft(m);
                const expired = m.state === "expired" || (days != null && days < 0);
                return (
                  <motion.div
                    key={m.id}
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    exit={{ opacity: 0 }}
                    className="min-w-0 px-3"
                  >
                    {index > 0 && <div className="mx-2 border-t border-dashed border-border" />}
                    <div className="flex min-w-0 items-start gap-2.5 py-3">
                      <span
                        className={cn(
                          "mt-1.5 h-2 w-2 shrink-0 rounded-full",
                          m.state === "ok" && "bg-running",
                          (m.state === "expiring" || m.state === "error") && "bg-warn",
                          expired && "bg-error",
                          m.state === "idle" && "bg-faint/40"
                        )}
                      />
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="min-w-0 font-mono text-[12px] [overflow-wrap:anywhere]">{m.host.includes(":") ? `[${m.host}]` : m.host}{m.port !== 443 ? `:${m.port}` : ""}</span>
                          <Badge variant={m.state === "error" || expired ? "error" : days == null ? "outline" : days <= 30 ? "warn" : "running"} className="text-[10px]">
                            {m.state === "error" ? t("monitor.checkFailed") : expired ? t("monitor.expired") : days == null ? t("monitor.neverChecked") : `${days} ${t("tls.daysLeft")}`}
                          </Badge>
                        </div>
                        {m.lastError && <p className="mt-1 text-[11px] text-error [overflow-wrap:anywhere]">{m.lastError}</p>}
                        {m.issuer && <p className="mt-1 text-[10.5px] text-muted [overflow-wrap:anywhere]">{m.state === "error" ? `${t("monitor.previousResult")} · ` : ""}{m.issuer}</p>}
                        {m.expiresAt != null && <p className="mt-1 text-[10.5px] text-faint [overflow-wrap:anywhere]">{m.state === "error" ? `${t("monitor.previousResult")} · ` : ""}{t("monitor.expiresAt")} {new Date(m.expiresAt).toLocaleString()}</p>}
                        {m.notificationError && <p className="mt-1 text-[11px] text-warn [overflow-wrap:anywhere]">{m.notificationError}</p>}
                        <div className="mt-2 flex flex-wrap items-center gap-2">
                          <span className="text-[10px] text-faint [overflow-wrap:anywhere]">{m.lastChecked ? `${t("monitor.lastChecked")} ${new Date(m.lastChecked).toLocaleString()}` : t("monitor.neverChecked")}</span>
                          <Button
                            size="icon-sm"
                            variant="ghost"
                            title={t("monitor.checkNow")}
                            aria-label={`${t("monitor.checkNow")} ${m.host}:${m.port}`}
                            disabled={checking.has(m.id) || adding}
                            onClick={() => checkNow(m)}
                          >
                            {checking.has(m.id) ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <RefreshCw className="h-3.5 w-3.5" />}
                          </Button>
                          <Button
                            size="icon-sm"
                            variant="ghost"
                            className="text-faint hover:text-error"
                            title={t("common.delete")}
                            aria-label={`${t("common.delete")} ${m.host}:${m.port}`}
                            disabled={deleting}
                            onClick={() => { setDeleteError(null); setRemoving(m); }}
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </Button>
                        </div>
                      </div>
                    </div>
                  </motion.div>
                );
              })}
            </AnimatePresence>
          </div>
        ) : null}
        {/* 告警推送：全局设置（钉钉/企微/飞书/通用 webhook），到期/异常跃迁时转发 */}
        <MonitorNotifySetting />

        <p className="text-[10.5px] text-faint">{t("monitor.autoHint")}</p>
      </CardContent>

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => !o && !deletingRef.current && setRemoving(null)}
        title={`${t("common.delete")} · ${removing?.host ?? ""}`}
        description={t("monitor.deleteHint")}
        danger
        confirmText={t("common.delete")}
        loading={deleting}
        onConfirm={async () => {
          if (!removing || deletingRef.current) return;
          deletingRef.current = true; setDeleting(true); setDeleteError(null);
          try {
            await api.certMonitorDelete(removing.id);
            toast.success(t("monitor.deleted"));
            setRemoving(null);
          } catch (e) {
            setDeleteError(normalizeError(e));
          } finally {
            await invalidate(); deletingRef.current = false; setDeleting(false);
          }
        }}
      >{deleteError && <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{deleteError.message}</p>}</ConfirmDialog>
    </Card>
  );
}
