"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import { Bell, Globe, Loader2, Plus, RefreshCw, Trash2 } from "lucide-react";
import type { CertMonitor } from "@nsb/schema";
import { useT } from "@/lib/store";
import { toastError, useSettings } from "@/lib/hooks";
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
  const { data: settings } = useSettings();
  const [kind, setKind] = React.useState("none");
  const [url, setUrl] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [open, setOpen] = React.useState(false);

  React.useEffect(() => {
    if (settings) {
      const rec = settings as unknown as Record<string, unknown>;
      setKind((rec.monitorNotifyKind as string) ?? "none");
      setUrl((rec.monitorNotifyUrl as string) ?? "");
    }
  }, [settings]);

  const save = async () => {
    setBusy(true);
    try {
      await api.setSetting("monitorNotifyKind", kind);
      await api.setSetting("monitorNotifyUrl", url.trim());
      toast.success(t("monitor.notifySaved"));
      qc.invalidateQueries({ queryKey: ["settings"] });
      setOpen(false);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  if (!open) {
    const configured = kind && kind !== "none" && url.trim();
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
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
        <Button size="sm" variant="ghost" className="h-6 px-1.5" onClick={() => setOpen(false)}>x</Button>
      </div>
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
        <Select value={kind} onValueChange={setKind}>
          <SelectTrigger className="h-8 text-[12px]"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="none">{t("certauto.notify.none")}</SelectItem>
            <SelectItem value="generic">{t("certauto.notify.generic")}</SelectItem>
            <SelectItem value="dingtalk">{t("certauto.notify.dingtalk")}</SelectItem>
            <SelectItem value="wecom">{t("certauto.notify.wecom")}</SelectItem>
            <SelectItem value="feishu">{t("certauto.notify.feishu")}</SelectItem>
          </SelectContent>
        </Select>
        <Input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder={t("certauto.notifyUrl")}
          className="font-mono text-[12px]"
        />
      </div>
      <div className="flex items-center gap-2">
        <Button size="sm" variant="secondary" disabled={busy} onClick={save}>
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
  const { data: monitors = [] } = useQuery({
    queryKey: ["certmonitors"],
    queryFn: api.certMonitorList,
    refetchInterval: 60_000,
    initialData: [],
    initialDataUpdatedAt: 0,
  });

  const [host, setHost] = React.useState("");
  const [adding, setAdding] = React.useState(false);
  const [checkingId, setCheckingId] = React.useState<string | null>(null);
  const [removing, setRemoving] = React.useState<CertMonitor | null>(null);

  const add = async () => {
    if (!host.trim()) return;
    setAdding(true);
    try {
      // 支持 host:port（盯非 443 的服务：NAS、邮件、自建面板…）
      const raw = host.trim().replace(/^https?:\/\//, "");
      const [h, portStr] = raw.split(":");
      const port = portStr && /^\d+$/.test(portStr) ? Number(portStr) : 443;
      const m = await api.certMonitorAdd({
        id: "", name: "", host: h, port,
        state: "idle", issuer: "", lastError: "",
        expiresAt: null, lastChecked: null,
        createdAt: 0, updatedAt: 0,
      });
      // 加完立刻查一次，让用户马上看到结果
      await api.certMonitorCheck(m.id);
      setHost("");
      invalidate();
    } catch (e) {
      toastError(e);
    } finally {
      setAdding(false);
    }
  };

  const checkNow = async (m: CertMonitor) => {
    setCheckingId(m.id);
    try {
      await api.certMonitorCheck(m.id);
      invalidate();
    } catch (e) {
      toastError(e);
    } finally {
      setCheckingId(null);
    }
  };

  const daysLeft = (m: CertMonitor) =>
    m.expiresAt != null ? Math.max(0, Math.round((m.expiresAt - Date.now()) / 86400_000)) : null;

  return (
    <Card>
      <CardHeader className="flex-row items-center gap-3">
        <div className="flex h-9 w-9 items-center justify-center rounded-lg border border-border bg-card-2/60">
          <Globe className="h-4 w-4 text-primary" strokeWidth={1.8} />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{t("monitor.title")}</CardTitle>
          <CardDescription className="text-[11px]">{t("monitor.hint")}</CardDescription>
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {/* 添加行 */}
        <div className="flex gap-2">
          <Input
            value={host}
            onChange={(e) => setHost(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && add()}
            placeholder={t("monitor.placeholder")}
            className="font-mono text-[12px]"
          />
          <Button variant="secondary" disabled={adding || !host.trim()} onClick={add}>
            {adding ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Plus className="h-3.5 w-3.5" />}
            {t("monitor.add")}
          </Button>
        </div>

        {monitors.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-[11.5px] text-faint">
            {t("monitor.empty")}
          </p>
        ) : (
          <div className="flex flex-col divide-y divide-border rounded-lg border border-border bg-card-2/20">
            <AnimatePresence initial={false}>
              {monitors.map((m) => {
                const days = daysLeft(m);
                const expired = m.state === "expired";
                return (
                  <motion.div
                    key={m.id}
                    layout
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    exit={{ opacity: 0 }}
                    className="flex items-center gap-2.5 px-3 py-2"
                  >
                    <span
                      className={cn(
                        "h-2 w-2 shrink-0 rounded-full",
                        m.state === "ok" && "bg-running",
                        (m.state === "expiring" || m.state === "error") && "bg-warn",
                        expired && "bg-error",
                        m.state === "idle" && "bg-faint/40"
                      )}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="truncate font-mono text-[12px]">{m.host}</span>
                        {m.port !== 443 && <span className="shrink-0 font-mono text-[10.5px] text-faint">:{m.port}</span>}
                        {days != null && (
                          <Badge variant={expired ? "error" : days < 7 ? "error" : days < 30 ? "warn" : "running"} className="text-[10px]">
                            {expired ? t("monitor.expired") : `${days} ${t("tls.daysLeft")}`}
                          </Badge>
                        )}
                      </div>
                      <p className="truncate text-[10.5px] text-faint">
                        {m.issuer
                          ? m.issuer
                          : m.lastError
                            ? m.lastError
                            : t("monitor.neverChecked")}
                      </p>
                    </div>
                    <span className="shrink-0 text-[10px] text-faint">
                      {m.lastChecked ? new Date(m.lastChecked).toLocaleDateString() : ""}
                    </span>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      title={t("monitor.checkNow")}
                      disabled={checkingId === m.id}
                      onClick={() => checkNow(m)}
                    >
                      {checkingId === m.id ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <RefreshCw className="h-3.5 w-3.5" />}
                    </Button>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      className="text-faint hover:text-error"
                      title={t("common.delete")}
                      onClick={() => setRemoving(m)}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </motion.div>
                );
              })}
            </AnimatePresence>
          </div>
        )}
        {/* 告警推送：全局设置（钉钉/企微/飞书/通用 webhook），到期/异常跃迁时转发 */}
        <MonitorNotifySetting />

        <p className="text-[10.5px] text-faint">{t("monitor.autoHint")}</p>
      </CardContent>

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={`${t("common.delete")} · ${removing?.host ?? ""}`}
        description={t("monitor.deleteHint")}
        danger
        confirmText={t("common.delete")}
        onConfirm={async () => {
          if (!removing) return;
          try {
            await api.certMonitorDelete(removing.id);
            toast.success(t("monitor.deleted"));
          } catch (e) {
            toastError(e);
          } finally {
            setRemoving(null);
            invalidate();
          }
        }}
      />
    </Card>
  );
}
