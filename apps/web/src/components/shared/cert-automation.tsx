"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  Cloud,
  Globe,
  History,
  Loader2,
  Pencil,
  Plus,
  Rocket,
  Square,
  Trash2,
  Workflow,
  CircleCheck,
  CircleAlert,
} from "lucide-react";
import type { CertAutomation, CertRunRecord, DeployResult, DeployTarget } from "@nsb/schema";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog, EmptyState, CopyButton } from "@/components/shared/misc";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/* ============================================================
   证书自动化（对齐 certd 的玩法，本地化成桌面端内置）：
   填域名 + DNS 凭据 → ACME 签发（DNS-01，支持通配符）→ 自动写进
   本地站点 / 推到宝塔 / 1Panel / 阿里云 / 腾讯云 / SSH 主机 / 本地
   目录 → 到期前自动续签，失败按策略重试并发 Webhook 通知。
   ============================================================ */

const CAs = [
  { value: "letsencrypt", labelKey: "certauto.ca.letsencrypt" },
  { value: "letsencrypt-staging", labelKey: "certauto.ca.staging" },
  { value: "zerossl", labelKey: "certauto.ca.zerossl" },
  { value: "google", labelKey: "certauto.ca.google" },
  { value: "buypass", labelKey: "certauto.ca.buypass" },
] as const;

/** 需要 EAB（外部账号绑定）的 CA */
const EAB_CAS = ["zerossl", "google", "buypass"];

const DNS_KINDS = [
  { value: "aliyun", labelKey: "certauto.dns.aliyun" },
  { value: "cloudflare", labelKey: "certauto.dns.cloudflare" },
  { value: "dnspod", labelKey: "certauto.dns.dnspod" },
  { value: "huawei", labelKey: "certauto.dns.huawei" },
  { value: "godaddy", labelKey: "certauto.dns.godaddy" },
  { value: "digitalocean", labelKey: "certauto.dns.digitalocean" },
  { value: "porkbun", labelKey: "certauto.dns.porkbun" },
  { value: "manual", labelKey: "certauto.dns.manual" },
] as const;

const TARGET_KINDS = ["btpanel", "onepanel", "aliyun", "tencent", "ssh", "local", "synology", "k8s", "qiniu", "hwssl"] as const;

/** 各部署目标需要的参数 */
const TARGET_FIELDS: Record<string, { key: string; labelKey: string; secret?: boolean }[]> = {
  btpanel: [
    { key: "url", labelKey: "certauto.field.url" },
    { key: "apiSk", labelKey: "certauto.field.apiSk", secret: true },
    { key: "siteName", labelKey: "certauto.field.siteName" },
  ],
  onepanel: [
    { key: "url", labelKey: "certauto.field.url" },
    { key: "token", labelKey: "certauto.field.apiSk", secret: true },
  ],
  aliyun: [
    { key: "accessKeyId", labelKey: "certauto.field.accessKeyId" },
    { key: "accessKeySecret", labelKey: "certauto.field.accessKeySecret", secret: true },
    { key: "region", labelKey: "certauto.field.region" },
  ],
  tencent: [
    { key: "secretId", labelKey: "certauto.field.accessKeyId" },
    { key: "secretKey", labelKey: "certauto.field.accessKeySecret", secret: true },
    { key: "region", labelKey: "certauto.field.region" },
  ],
  synology: [
    { key: "url", labelKey: "certauto.field.url" },
    { key: "account", labelKey: "certauto.field.account" },
    { key: "password", labelKey: "certauto.field.password", secret: true },
  ],
  k8s: [
    { key: "serverUrl", labelKey: "certauto.field.k8sServer" },
    { key: "token", labelKey: "certauto.field.k8sToken", secret: true },
    { key: "namespace", labelKey: "certauto.field.k8sNamespace" },
    { key: "secretName", labelKey: "certauto.field.k8sSecret" },
    { key: "insecure", labelKey: "certauto.field.k8sInsecure" },
  ],
  qiniu: [
    { key: "accessKey", labelKey: "certauto.field.accessKeyId" },
    { key: "secretKey", labelKey: "certauto.field.accessKeySecret", secret: true },
  ],
  hwssl: [
    { key: "accessKeyId", labelKey: "certauto.field.accessKeyId" },
    { key: "accessKeySecret", labelKey: "certauto.field.accessKeySecret", secret: true },
    { key: "region", labelKey: "certauto.field.region" },
  ],
  ssh: [
    { key: "host", labelKey: "certauto.field.host" },
    { key: "port", labelKey: "certauto.field.port" },
    { key: "user", labelKey: "certauto.field.user" },
    { key: "password", labelKey: "certauto.field.password", secret: true },
    { key: "keyPath", labelKey: "certauto.field.sshKeyPath" },
    { key: "certPath", labelKey: "certauto.field.remoteCertPath" },
    { key: "keyPath", labelKey: "certauto.field.remoteKeyPath" },
    { key: "script", labelKey: "certauto.field.script" },
  ],
  local: [
    { key: "certPath", labelKey: "certauto.field.localCertPath" },
    { key: "keyPath", labelKey: "certauto.field.localKeyPath" },
    { key: "script", labelKey: "certauto.field.script" },
  ],
};

const KEY_ALGS = [
  { value: "ec256", labelKey: "certauto.keyalg.ec256" },
  { value: "ec384", labelKey: "certauto.keyalg.ec384" },
  { value: "rsa2048", labelKey: "certauto.keyalg.rsa2048" },
  { value: "rsa3072", labelKey: "certauto.keyalg.rsa3072" },
  { value: "rsa4096", labelKey: "certauto.keyalg.rsa4096" },
] as const;

const NOTIFY_KINDS = [
  { value: "none", labelKey: "certauto.notify.none" },
  { value: "email", labelKey: "certauto.notify.email" },
  { value: "generic", labelKey: "certauto.notify.generic" },
  { value: "dingtalk", labelKey: "certauto.notify.dingtalk" },
  { value: "wecom", labelKey: "certauto.notify.wecom" },
  { value: "feishu", labelKey: "certauto.notify.feishu" },
] as const;

function useCertAutos() {
  return useQuery({
    queryKey: ["certautos"],
    queryFn: api.certAutoList,
    refetchInterval: 15_000,
    initialData: [],
    initialDataUpdatedAt: 0,
  });
}

export function CertAutomationSection() {
  const t = useT();
  const invalidate = useInvalidateSafe();
  const { data: autos = [] } = useCertAutos();
  const [creating, setCreating] = React.useState(false);
  const [editing, setEditing] = React.useState<CertAutomation | null>(null);
  const [removing, setRemoving] = React.useState<CertAutomation | null>(null);
  const [issuingId, setIssuingId] = React.useState<string | null>(null);
  const [historyOf, setHistoryOf] = React.useState<CertAutomation | null>(null);

  /** 手动立即签发：ACME 全流程要 1–2 分钟，按钮转圈并提示 */
  const issueNow = async (a: CertAutomation) => {
    setIssuingId(a.id);
    invalidate();
    try {
      await api.certAutoIssue(a.id);
      toast.success(t("certauto.issuedOk"), { description: a.domains.join(", ") });
    } catch (e) {
      toastError(e, t("certauto.issueFailed"));
    } finally {
      setIssuingId(null);
      invalidate();
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <p className="text-[11.5px] text-faint">{t("certauto.sectionHint")}</p>
        <Button size="sm" onClick={() => setCreating(true)}>
          <Plus className="h-3.5 w-3.5" /> {t("certauto.new")}
        </Button>
      </div>

      {autos.length === 0 ? (
        <EmptyState
          icon={Workflow}
          title={t("certauto.empty")}
          hint={t("certauto.emptyHint")}
          action={
            <Button onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" /> {t("certauto.new")}
            </Button>
          }
        />
      ) : (
        <div className="grid grid-cols-1 gap-3 xl:grid-cols-2">
          <AnimatePresence initial={false}>
            {autos.map((a) => (
              <AutomationCard
                key={a.id}
                a={a}
                busy={issuingId === a.id}
                onIssue={() => issueNow(a)}
                onEdit={() => setEditing(a)}
                onRemove={() => setRemoving(a)}
                onHistory={() => setHistoryOf(a)}
              />
            ))}
          </AnimatePresence>
        </div>
      )}

      <AutomationDialog
        open={creating || editing !== null}
        automation={editing}
        onOpenChange={(o) => {
          if (!o) {
            setCreating(false);
            setEditing(null);
          }
        }}
      />

      <RunHistoryDrawer automation={historyOf} onClose={() => setHistoryOf(null)} />

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={`${t("common.delete")} · ${removing?.name ?? ""}`}
        description={t("certauto.deleteHint")}
        danger
        confirmText={t("common.delete")}
        onConfirm={async () => {
          if (!removing) return;
          try {
            await api.certAutoDelete(removing.id);
            toast.success(t("certauto.deleted"));
          } catch (e) {
            toastError(e);
          } finally {
            setRemoving(null);
            invalidate();
          }
        }}
      />
    </div>
  );
}

function AutomationCard({
  a,
  busy,
  onIssue,
  onEdit,
  onRemove,
  onHistory,
}: {
  a: CertAutomation;
  busy: boolean;
  onIssue: () => void;
  onEdit: () => void;
  onRemove: () => void;
  onHistory: () => void;
}) {
  const t = useT();
  const invalidate = useInvalidateSafe();
  const daysLeft = a.expiresAt ? Math.max(0, Math.round((a.expiresAt - Date.now()) / 86400_000)) : null;

  const toggle = async (enabled: boolean) => {
    try {
      await api.certAutoSetEnabled(a.id, enabled);
      invalidate();
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <motion.div layout initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, scale: 0.98 }}>
      <Card className={cn("flex h-full flex-col gap-3 p-4", a.enabled && a.state === "ok" && "border-running/25")}>
        {/* 头：名称 + 状态 + 启用开关 */}
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <span className="truncate text-[13.5px] font-medium">{a.name}</span>
              <StateBadge state={a.state} />
            </div>
            <p className="mt-0.5 truncate font-mono text-[11px] text-faint">{a.domains.join(" · ")}</p>
          </div>
          <Switch checked={a.enabled} onCheckedChange={toggle} aria-label={t("common.start")} />
        </div>

        {/* 元信息 */}
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant="outline" className="text-[10px]">
            <Cloud className="h-3 w-3" /> {t(`certauto.ca.${a.ca}` as never)}
          </Badge>
          <Badge variant="outline" className="text-[10px]">
            <Globe className="h-3 w-3" /> {t(`certauto.dns.${a.dns.kind}` as never)}
          </Badge>
          {a.keyAlg !== "ec256" && (
            <Badge variant="outline" className="text-[10px]">
              {t(`certauto.keyalg.${a.keyAlg}` as never)}
            </Badge>
          )}
          {a.deployLocal && (
            <Badge variant="default" className="text-[10px]">
              {t("certauto.deployLocal")}
            </Badge>
          )}
          {daysLeft != null && (
            <Badge variant={daysLeft < 7 ? "error" : daysLeft < 30 ? "warn" : "running"} className="text-[10px]">
              {daysLeft} {t("tls.daysLeft")}
            </Badge>
          )}
        </div>

        {/* 续签节奏 */}
        <p className="text-[10.5px] text-faint">
          {a.lastRunAt
            ? `${t("certauto.lastRun")} ${new Date(a.lastRunAt).toLocaleString()}`
            : t("certauto.neverRun")}
          {a.nextRenewAt > 0 && a.nextRenewAt < 8_000_000_000_000
            ? ` · ${t("certauto.nextRenew")} ${new Date(a.nextRenewAt).toLocaleDateString()}`
            : ""}
          {a.failCount > 0 ? ` · ${t("certauto.failCount")} ${a.failCount}` : ""}
        </p>

        {/* 手动 DNS 等待：把要加的 TXT 记录摆出来（certd 手动模式的核心交互） */}
        {a.state === "manual_wait" && a.manualRecords.length > 0 && (
          <div className="flex flex-col gap-1.5 rounded-lg border border-info/30 bg-info-soft px-2.5 py-2">
            <p className="text-[11px] font-medium text-info">{t("certauto.manualWaitTitle")}</p>
            {a.manualRecords.map((r) => (
              <div key={r.name} className="flex items-center gap-2">
                <code className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-secondary">{r.name}</code>
                <span className="text-faint">→</span>
                <code className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-secondary">{r.value}</code>
                <CopyButton text={r.value} />
              </div>
            ))}
            <p className="text-[10.5px] text-info/80">{t("certauto.manualWaitHint")}</p>
          </div>
        )}

        {/* 失败原因就地可见 */}
        {a.state === "error" && a.lastError && (
          <p className="rounded-lg border border-error/25 bg-error/10 px-2.5 py-1.5 text-[11px] text-error">
            {a.lastError}
          </p>
        )}

        {/* 部署目标 + 各自最近一次结果 */}
        {a.targets.length > 0 && (
          <div className="flex flex-col gap-1">
            {a.targets.map((tg) => (
              <div key={tg.id} className="flex items-center gap-2 text-[11px]">
                <TargetDot result={tg.lastResult} />
                <span className="text-faint">{t(`certauto.target.${tg.kind}` as never)}</span>
                <span className="truncate text-secondary">{tg.name}</span>
                {tg.lastResult && !tg.lastResult.ok && (
                  <span className="truncate text-error/80" title={tg.lastResult.message}>
                    {tg.lastResult.message}
                  </span>
                )}
              </div>
            ))}
          </div>
        )}

        {/* 操作 */}
        <div className="mt-auto flex items-center gap-2 border-t border-border pt-3">
          <Button size="sm" className="flex-1" disabled={busy || !a.enabled} onClick={onIssue}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Rocket className="h-3.5 w-3.5" />}
            {a.issuedAt ? t("certauto.renewNow") : t("certauto.issueNow")}
          </Button>
          <Button size="icon-sm" variant="ghost" title={t("certauto.history")} onClick={onHistory}>
            <History className="h-3.5 w-3.5" />
          </Button>
          <Button size="icon-sm" variant="ghost" title={t("stack.edit")} onClick={onEdit}>
            <Pencil className="h-3.5 w-3.5" />
          </Button>
          <Button size="icon-sm" variant="ghost" className="text-error/80 hover:text-error" title={t("common.delete")} onClick={onRemove}>
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
        {busy && <p className="text-[10.5px] text-faint">{t("certauto.issueBusyHint")}</p>}
      </Card>
    </motion.div>
  );
}

/* ============ 执行历史（certd 的执行日志） ============ */

function RunHistoryDrawer({ automation, onClose }: { automation: CertAutomation | null; onClose: () => void }) {
  const t = useT();
  return (
    <Dialog open={automation !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[85vh] max-w-xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("certauto.history")}</DialogTitle>
          <DialogDescription>{automation?.domains.join(" · ")}</DialogDescription>
        </DialogHeader>
        {(!automation || automation.runs.length === 0) && (
          <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-[12px] text-faint">
            {t("certauto.noRuns")}
          </p>
        )}
        <div className="flex flex-col gap-2">
          {automation?.runs.map((r, i) => (
            <RunItem key={r.at} run={r} defaultOpen={i === 0} />
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function RunItem({ run, defaultOpen }: { run: CertRunRecord; defaultOpen: boolean }) {
  const [open, setOpen] = React.useState(defaultOpen);
  return (
    <div className="rounded-lg border border-border">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-[12px] transition-colors hover:bg-card-2/40"
      >
        {run.ok ? <CircleCheck className="h-3.5 w-3.5 shrink-0 text-running" /> : <CircleAlert className="h-3.5 w-3.5 shrink-0 text-error" />}
        <span className="flex-1 truncate">{run.message}</span>
        <span className="shrink-0 text-[10.5px] text-faint">{new Date(run.at).toLocaleString()}</span>
      </button>
      {open && run.log.length > 0 && (
        <pre className="max-h-52 overflow-y-auto border-t border-border bg-card-2/30 px-3 py-2 font-mono text-[10.5px] leading-relaxed text-secondary">
          {run.log.join("\n")}
        </pre>
      )}
    </div>
  );
}

function StateBadge({ state }: { state: string }) {
  const t = useT();
  const map: Record<string, { variant: "running" | "warn" | "error" | "muted" | "info"; key: string }> = {
    ok: { variant: "running", key: "certauto.state.ok" },
    issuing: { variant: "warn", key: "certauto.state.issuing" },
    error: { variant: "error", key: "certauto.state.error" },
    idle: { variant: "muted", key: "certauto.state.idle" },
    manual_wait: { variant: "info", key: "certauto.state.manualWait" },
    manual_due: { variant: "warn", key: "certauto.state.manualDue" },
  };
  const v = map[state] ?? map.idle;
  return <Badge variant={v.variant} className="text-[10px]">{t(v.key as never)}</Badge>;
}

function TargetDot({ result }: { result?: DeployResult | null }) {
  if (!result) return <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-faint/40" />;
  return result.ok ? (
    <CircleCheck className="h-3 w-3 shrink-0 text-running" />
  ) : (
    <CircleAlert className="h-3 w-3 shrink-0 text-error" />
  );
}

/* ============ 创建 / 编辑 ============ */

function emptySmtp() {
  return { host: "", port: 587, username: "", password: "", from: "", to: "", implicitTls: false };
}

function emptyAutomation(): CertAutomation {
  return {
    id: "",
    name: "",
    domains: [],
    email: "",
    ca: "letsencrypt",
    dns: { kind: "aliyun", accessKey: "", secret: "" },
    deployLocal: true,
    targets: [],
    enabled: true,
    state: "idle",
    lastError: "",
    certId: null,
    issuedAt: null,
    expiresAt: null,
    nextRenewAt: 0,
    lastRunAt: 0,
    keyAlg: "ec256",
    eabKid: "",
    eabHmacKey: "",
    dnsWaitSec: 0,
    cnameTarget: "",
    renewDaysAhead: 30,
    retryTimes: 3,
    retryIntervalMin: 30,
    failCount: 0,
    notifyKind: "none",
    notifyUrl: "",
    notifySmtp: null,
    runs: [],
    manualRecords: [],
    createdAt: 0,
    updatedAt: 0,
  };
}

function AutomationDialog({
  open,
  automation,
  onOpenChange,
}: {
  open: boolean;
  automation: CertAutomation | null;
  onOpenChange: (o: boolean) => void;
}) {
  const t = useT();
  const invalidate = useInvalidateSafe();
  const [form, setForm] = React.useState<CertAutomation | null>(null);
  const [busy, setBusy] = React.useState(false);

  React.useEffect(() => {
    if (!open) return;
    setForm(automation ? structuredClone(automation) : emptyAutomation());
  }, [open, automation]);

  if (!form) return null;
  const patch = (p: Partial<CertAutomation>) => setForm({ ...form, ...p });

  const save = async () => {
    if (form.domains.length === 0) {
      toast.error(t("certauto.errDomains"));
      return;
    }
    setBusy(true);
    try {
      await api.certAutoSave(form);
      toast.success(t("certauto.saved"), {
        description: t("certauto.savedHint"),
      });
      onOpenChange(false);
      invalidate();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const setTargetConfig = (tid: string, key: string, value: string) =>
    patch({
      targets: form.targets.map((tg) =>
        tg.id === tid ? { ...tg, config: { ...tg.config, [key]: value } } : tg
      ),
    });

  const addTarget = (kind: (typeof TARGET_KINDS)[number]) =>
    patch({
      targets: [
        ...form.targets,
        {
          id: `t-${Date.now()}`,
          kind,
          name: t(`certauto.target.${kind}` as never),
          config: {},
          lastResult: null,
        } satisfies DeployTarget,
      ],
    });

  const needEab = EAB_CAS.includes(form.ca);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[88vh] max-w-xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{automation ? t("certauto.edit") : t("certauto.new")}</DialogTitle>
          <DialogDescription>{t("certauto.editorHint")}</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          {/* 域名 */}
          <div className="flex flex-col gap-1.5">
            <Label>{t("certauto.domains")}</Label>
            <Input
              value={form.domains.join(", ")}
              onChange={(e) =>
                patch({ domains: e.target.value.split(/[,，\s]+/).filter(Boolean) })
              }
              placeholder="example.com, www.example.com, *.example.com"
              className="font-mono text-[12px]"
            />
            <p className="text-[10.5px] text-faint">{t("certauto.domainsHint")}</p>
          </div>

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="flex flex-col gap-1.5">
              <Label>{t("certauto.ca")}</Label>
              <Select value={form.ca} onValueChange={(v) => patch({ ca: v })}>
                <SelectTrigger className="text-[12px]"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {CAs.map((c) => (
                    <SelectItem key={c.value} value={c.value}>{t(c.labelKey)}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex flex-col gap-1.5">
              <Label>{t("certauto.email")}</Label>
              <Input
                value={form.email}
                onChange={(e) => patch({ email: e.target.value })}
                placeholder="me@example.com"
                className="text-[12px]"
              />
            </div>
          </div>

          {/* EAB：ZeroSSL / Google / BuyPass 需要 */}
          {needEab && (
            <div className="flex flex-col gap-2 rounded-xl border border-info/25 bg-info-soft p-3">
              <Label className="text-[12px]">{t("certauto.eabTitle")}</Label>
              <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                <Input
                  value={form.eabKid}
                  onChange={(e) => patch({ eabKid: e.target.value })}
                  placeholder={t("certauto.field.eabKid")}
                  className="font-mono text-[12px]"
                />
                <Input
                  type="password"
                  value={form.eabHmacKey}
                  onChange={(e) => patch({ eabHmacKey: e.target.value })}
                  placeholder={t("certauto.field.eabHmacKey")}
                  className="font-mono text-[12px]"
                />
              </div>
            </div>
          )}

          <Separator />

          {/* DNS 服务商 */}
          <div className="flex flex-col gap-2">
            <Label>{t("certauto.dnsTitle")}</Label>
            {form.dns.kind === "manual" && (
              <p className="rounded-lg border border-info/25 bg-info-soft px-2.5 py-2 text-[11px] text-info">
                {t("certauto.manualHint")}
              </p>
            )}
            <Select value={form.dns.kind} onValueChange={(v) => patch({ dns: { ...form.dns, kind: v } })}>
              <SelectTrigger className="text-[12px]"><SelectValue /></SelectTrigger>
              <SelectContent>
                {DNS_KINDS.map((d) => (
                  <SelectItem key={d.value} value={d.value}>{t(d.labelKey)}</SelectItem>
                ))}
              </SelectContent>
            </Select>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              <Input
                value={form.dns.accessKey}
                onChange={(e) => patch({ dns: { ...form.dns, accessKey: e.target.value } })}
                placeholder={
                  form.dns.kind === "cloudflare"
                    ? t("certauto.field.cfToken")
                    : form.dns.kind === "digitalocean"
                      ? t("certauto.field.cfToken")
                      : t("certauto.field.accessKeyId")
                }
                className="font-mono text-[12px]"
              />
              {form.dns.kind !== "cloudflare" && form.dns.kind !== "digitalocean" && (
                <Input
                  type="password"
                  value={form.dns.secret}
                  onChange={(e) => patch({ dns: { ...form.dns, secret: e.target.value } })}
                  placeholder={t("certauto.field.accessKeySecret")}
                  className="font-mono text-[12px]"
                />
              )}
            </div>
            <p className="text-[10.5px] text-faint">{t("certauto.dnsPermHint")}</p>
          </div>

          <Separator />

          {/* 本地站点 */}
          <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
            <div className="flex flex-col gap-0.5">
              <span className="text-[12.5px] font-medium">{t("certauto.deployLocal")}</span>
              <span className="text-[11px] text-faint">{t("certauto.deployLocalHint")}</span>
            </div>
            <Switch checked={form.deployLocal} onCheckedChange={(v) => patch({ deployLocal: v })} />
          </div>

          {/* 部署目标 */}
          <div className="flex flex-col gap-2">
            <Label>{t("certauto.targets")} ({form.targets.length})</Label>
            {form.targets.map((tg) => (
              <div key={tg.id} className="flex flex-col gap-2 rounded-xl border border-border p-3">
                <div className="flex items-center gap-2">
                  <Badge variant="outline" className="text-[10px]">{t(`certauto.target.${tg.kind}` as never)}</Badge>
                  <Input
                    value={tg.name}
                    onChange={(e) =>
                      patch({ targets: form.targets.map((x) => (x.id === tg.id ? { ...x, name: e.target.value } : x)) })
                    }
                    className="h-7 flex-1 text-[12px]"
                    placeholder={t("certauto.targetName")}
                  />
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    className="text-error/70 hover:text-error"
                    onClick={() => patch({ targets: form.targets.filter((x) => x.id !== tg.id) })}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </Button>
                </div>
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                  {(TARGET_FIELDS[tg.kind] ?? []).map((f) => (
                    <Input
                      key={f.key}
                      type={f.secret ? "password" : "text"}
                      value={tg.config[f.key] ?? ""}
                      onChange={(e) => setTargetConfig(tg.id, f.key, e.target.value)}
                      placeholder={t(f.labelKey as never)}
                      className="font-mono text-[12px]"
                    />
                  ))}
                </div>
                {tg.kind === "btpanel" && (
                  <p className="text-[10.5px] text-faint">{t("certauto.btHint")}</p>
                )}
                {tg.kind === "ssh" && (
                  <p className="text-[10.5px] text-faint">{t("certauto.sshHint")}</p>
                )}
              </div>
            ))}
            <div className="flex flex-wrap gap-1.5">
              {TARGET_KINDS.filter((k) => !form.targets.some((tg) => tg.kind === k)).map((k) => (
                <button
                  key={k}
                  type="button"
                  onClick={() => addTarget(k)}
                  className="flex items-center gap-1 rounded-md border border-border bg-card-2/50 px-2 py-1 text-[11.5px] text-secondary transition-colors hover:border-border-strong hover:text-foreground"
                >
                  <Plus className="h-3 w-3" /> {t(`certauto.target.${k}` as never)}
                </button>
              ))}
            </div>
          </div>

          <Separator />

          {/* 高级选项 */}
          <details className="rounded-xl border border-border px-3.5 py-3">
            <summary className="cursor-pointer text-[12.5px] font-medium text-secondary">
              {t("certauto.advTitle")}
            </summary>
            <div className="mt-3 flex flex-col gap-3">
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <div className="flex flex-col gap-1.5">
                  <Label>{t("certauto.keyAlg")}</Label>
                  <Select value={form.keyAlg} onValueChange={(v) => patch({ keyAlg: v })}>
                    <SelectTrigger className="text-[12px]"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {KEY_ALGS.map((k) => (
                        <SelectItem key={k.value} value={k.value}>{t(k.labelKey)}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label>{t("certauto.renewAhead")}</Label>
                  <Input
                    type="number"
                    min={1}
                    max={90}
                    value={form.renewDaysAhead}
                    onChange={(e) => patch({ renewDaysAhead: Number(e.target.value) || 30 })}
                    className="text-[12px]"
                  />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label>{t("certauto.dnsWait")}</Label>
                  <Input
                    type="number"
                    min={0}
                    max={600}
                    value={form.dnsWaitSec}
                    onChange={(e) => patch({ dnsWaitSec: Number(e.target.value) || 0 })}
                    className="text-[12px]"
                  />
                </div>
                <div className="flex flex-col gap-1.5 sm:col-span-2">
                  <Label>{t("certauto.cnameTitle")}</Label>
                  <Input
                    value={form.cnameTarget}
                    onChange={(e) => patch({ cnameTarget: e.target.value })}
                    placeholder="{domain}.acme.dnspod.cn"
                    className="font-mono text-[12px]"
                  />
                  <p className="text-[10.5px] text-faint">{t("certauto.cnameHint")}</p>
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label>{t("certauto.retryTimes")}</Label>
                  <Input
                    type="number"
                    min={1}
                    max={20}
                    value={form.retryTimes}
                    onChange={(e) => patch({ retryTimes: Number(e.target.value) || 3 })}
                    className="text-[12px]"
                  />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label>{t("certauto.retryInterval")}</Label>
                  <Input
                    type="number"
                    min={1}
                    max={1440}
                    value={form.retryIntervalMin}
                    onChange={(e) => patch({ retryIntervalMin: Number(e.target.value) || 30 })}
                    className="text-[12px]"
                  />
                </div>
              </div>

              {/* 通知 */}
              <div className="flex flex-col gap-2 rounded-lg bg-card-2/30 p-3">
                <Label>{t("certauto.notify")}</Label>
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                  <Select value={form.notifyKind || "none"} onValueChange={(v) => patch({ notifyKind: v })}>
                    <SelectTrigger className="text-[12px]"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {NOTIFY_KINDS.map((n) => (
                        <SelectItem key={n.value} value={n.value}>{t(n.labelKey)}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Input
                    value={form.notifyUrl}
                    onChange={(e) => patch({ notifyUrl: e.target.value })}
                    placeholder={t("certauto.notifyUrl")}
                    className="font-mono text-[12px]"
                  />
                </div>
                {form.notifyKind === "email" && (
                  <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                    <Input
                      value={form.notifySmtp?.host ?? ""}
                      onChange={(e) => patch({ notifySmtp: { ...(form.notifySmtp ?? emptySmtp()), host: e.target.value } })}
                      placeholder={t("certauto.smtp.host")}
                      className="font-mono text-[12px]"
                    />
                    <Input
                      type="number"
                      value={form.notifySmtp?.port ?? 587}
                      onChange={(e) => patch({ notifySmtp: { ...(form.notifySmtp ?? emptySmtp()), port: Number(e.target.value) || 587 } })}
                      placeholder="587"
                      className="font-mono text-[12px]"
                    />
                    <Input
                      value={form.notifySmtp?.username ?? ""}
                      onChange={(e) => patch({ notifySmtp: { ...(form.notifySmtp ?? emptySmtp()), username: e.target.value } })}
                      placeholder={t("certauto.smtp.user")}
                      className="font-mono text-[12px]"
                    />
                    <Input
                      type="password"
                      value={form.notifySmtp?.password ?? ""}
                      onChange={(e) => patch({ notifySmtp: { ...(form.notifySmtp ?? emptySmtp()), password: e.target.value } })}
                      placeholder={t("certauto.smtp.pass")}
                      className="font-mono text-[12px]"
                    />
                    <Input
                      value={form.notifySmtp?.to ?? ""}
                      onChange={(e) => patch({ notifySmtp: { ...(form.notifySmtp ?? emptySmtp()), to: e.target.value } })}
                      placeholder={t("certauto.smtp.to")}
                      className="font-mono text-[12px]"
                    />
                    <Select
                      value={form.notifySmtp?.implicitTls ? "tls" : "starttls"}
                      onValueChange={(v) =>
                        patch({ notifySmtp: { ...(form.notifySmtp ?? emptySmtp()), implicitTls: v === "tls" } })
                      }
                    >
                      <SelectTrigger className="h-8 text-[12px]"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="starttls">STARTTLS (587)</SelectItem>
                        <SelectItem value="tls">SSL/TLS (465)</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                )}
                <p className="text-[10.5px] text-faint">{t("certauto.notifyHint")}</p>
              </div>
            </div>
          </details>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            {t("common.cancel")}
          </Button>
          <Button onClick={save} disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Square className="h-3.5 w-3.5" />}
            {t("common.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Separator() {
  return <div className="h-px bg-border" />;
}

function useInvalidateSafe() {
  const qc = useQueryClient();
  return React.useCallback(() => {
    qc.invalidateQueries({ queryKey: ["certautos"] });
    qc.invalidateQueries({ queryKey: ["certs"] });
  }, [qc]);
}
