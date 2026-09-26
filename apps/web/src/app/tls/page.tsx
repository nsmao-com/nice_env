"use client";

import * as React from "react";
import { toast } from "sonner";
import { ShieldCheck, ShieldX, RefreshCw, Plus, Trash2, CalendarClock } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import type { CertRecord } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useCerts, toastError } from "@/lib/hooks";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import * as api from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ConfirmDialog } from "@/components/shared/misc";
import { EmptyState } from "@/components/shared/misc";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from "@/components/ui/dialog";
import { PageHeader } from "@/components/layout/app-shell";
import { CertHealthCard } from "@/components/shared/cert-health";
import { CertAutomationSection } from "@/components/shared/cert-automation";
import { CertMonitorCard } from "@/components/shared/cert-monitor-card";
import { cn } from "@/lib/utils";
import { KeyRound, Download, FolderArchive, Loader2 } from "lucide-react";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Workflow } from "lucide-react";

export default function TlsPage() {
  const t = useT();
  const lang = useUI((s) => s.lang);
  const formatDate = (value: number) => new Date(value).toLocaleDateString(lang === "zh" ? "zh-CN" : "en-US");
  const certQuery = useCerts();
  const certs = certQuery.data;
  const ready = certQuery.dataUpdatedAt > 0 && !certQuery.error;
  const queryClient = useQueryClient();
  const refresh = () => Promise.all(["certs", "cert-health", "cert-imported", "services", "sites"].map(
    (key) => queryClient.invalidateQueries({ queryKey: [key] })
  ));
  const [tab, setTab] = React.useState("local");
  const [issueOpen, setIssueOpen] = React.useState(false);
  const [reissueTarget, setReissueTarget] = React.useState<{ cert: CertRecord; action: "reissue" | "delete" } | null>(null);
  const [reissuing, setReissuing] = React.useState(false);
  const actionRef = React.useRef(false);
  const [actionError, setActionError] = React.useState<AppErrorShape | null>(null);
  const [trusting, setTrusting] = React.useState(false);
  const trustRef = React.useRef(false);
  const [trustError, setTrustError] = React.useState<AppErrorShape | null>(null);
  const [importing, setImporting] = React.useState(false);
  const importRef = React.useRef(false);
  const [pfxTarget, setPfxTarget] = React.useState<CertRecord | null>(null);
  const actionTrigger = React.useRef<HTMLButtonElement | null>(null);
  const issueTrigger = React.useRef<HTMLButtonElement | null>(null);
  const restoreActionFocus = (event: Event) => {
    event.preventDefault();
    const trigger = actionTrigger.current;
    (trigger?.isConnected && !trigger.disabled ? trigger : issueTrigger.current)?.focus();
  };
  const ca = certs.find((c) => c.kind === "ca");
  // 本机证书 = 自签(site) + ACME 签发(acme) 都算；CA 行单独展示
  const siteCerts = certs.filter((c) => c.kind === "site" || c.kind === "acme");

  const daysLeft = (ts: number) => Math.ceil((ts - Date.now()) / 86400_000);

  /** 从文件夹批量导入证书文件（certd 没导出配置、只剩证书文件时的迁移路） */
  const importCertDir = async () => {
    if (importRef.current) return;
    if (!isTauri) { toast.info(t("tls.desktopOnly")); return; }
    importRef.current = true;
    setImporting(true);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const dir = await open({ title: t("tls.importDirTitle"), directory: true, multiple: false });
      if (!dir || typeof dir !== "string") return;
      const r = await api.certImportDir(dir);
      const NL = String.fromCharCode(10);
      const desc = [
        ...r.imported.map((c) => `${c.subject}（剩 ${c.daysLeft} 天）`),
        ...r.skipped.slice(0, 6),
      ].join(NL);
      if (r.imported.length > 0) {
        toast.success(`${t("tls.importDirDone")}（${r.imported.length}）`, { description: desc, duration: 12000 });
      } else {
        toast.warning(t("tls.importDirNone"), { description: desc || t("tls.importDirNoneHint"), duration: 12000 });
      }
    } catch (e) {
      toastError(e, t("tls.importDirFailed"));
    } finally {
      await refresh();
      importRef.current = false;
      setImporting(false);
    }
  };

  return (
    <div className="pb-8">
      <PageHeader
        title={t("tls.title")}
        subtitle={t("tls.subtitle")}
        actions={
          <>
            <Button variant="secondary" disabled={importing} onClick={importCertDir}>
              <FolderArchive className="h-3.5 w-3.5" /> {t("tls.importDir")}
            </Button>
            <Button ref={issueTrigger} disabled={!ready || reissuing || trusting} onClick={(event) => { actionTrigger.current = event.currentTarget; setIssueOpen(true); }}>
              <Plus className="h-3.5 w-3.5" /> {t("tls.issueTitle")}
            </Button>
          </>
        }
      />

      <Tabs value={tab} onValueChange={setTab}>
        <TabsList className="mb-5 max-w-full flex-wrap">
          <TabsTrigger value="local">
            <ShieldCheck className="h-3.5 w-3.5" />
            {t("tls.tab.local")}
          </TabsTrigger>
          <TabsTrigger value="automation">
            <Workflow className="h-3.5 w-3.5" />
            {t("tls.tab.automation")}
          </TabsTrigger>
        </TabsList>

        {/* ============ 本机证书：CA / 自签站点证书 / 体检 ============ */}
        <TabsContent value="local" className="mt-0">
          <div className="flex flex-col gap-6">
          {/* 证书体检：放在页头下方，不要塞进 actions —— 那会变成 button 套 button */}
          <section>
            <CertHealthCard />
          </section>

          {/* 网站证书监控：盯任意站点/设备的证书到期（certd 的站点监控） */}
          <section>
            <CertMonitorCard />
          </section>

      {/* 根 CA */}
      {!ready && <div role={certQuery.error ? "alert" : "status"} className="flex flex-wrap items-center gap-3 rounded-xl border border-border p-3 text-xs [overflow-wrap:anywhere]">
        <p className="min-w-0 flex-1">{t(certQuery.error ? "tls.readFailed" : "common.loading")}</p>
        {certQuery.error && <Button size="sm" variant="secondary" disabled={certQuery.isFetching} onClick={() => void certQuery.refetch()}>{t("bulk.retry")}</Button>}
      </div>}
      {certQuery.error && <CertError error={normalizeError(certQuery.error)} />}
      <Card className="mb-6">
        <CardHeader className="flex-row flex-wrap items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-3">
            <div className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border ${ready && ca?.trusted ? "border-running/30 bg-running-soft" : "border-warn/30 bg-warn/10"}`}>
              {ready && ca?.trusted ? (
                <ShieldCheck className="h-5 w-5 text-running" strokeWidth={1.8} />
              ) : (
                <ShieldX className="h-5 w-5 text-warn" strokeWidth={1.8} />
              )}
            </div>
            <div className="min-w-0">
              <CardTitle className="text-[14px]">{t("tls.ca")}</CardTitle>
              <CardDescription className="mt-1 break-words font-mono text-[11px]">
                {ca ? `CN=${ca.subject} · ${formatDate(ca.notAfter)}` : t(ready ? "tls.caNotCreated" : "tls.statusUnknown")}
              </CardDescription>
            </div>
          </div>
          {ca && (
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant={ready && ca.trusted ? "running" : "warn"}>
                {t(!ready ? "tls.statusUnknown" : ca.trusted ? "tls.trusted" : "tls.notTrusted")}
              </Badge>
              {!ca.trusted && (
                <Button
                  disabled={!ready || trusting}
                  onClick={async () => {
                    if (trustRef.current) return;
                    trustRef.current = true;
                    setTrusting(true);
                    setTrustError(null);
                    try {
                      await api.trustCa();
                      toast.success(t("tls.caTrusted"));
                    } catch (e) {
                      setTrustError(normalizeError(e));
                    } finally {
                      await refresh();
                      trustRef.current = false;
                      setTrusting(false);
                    }
                  }}
                >
                  <ShieldCheck className="h-3.5 w-3.5" /> {t("tls.trust")}
                </Button>
              )}
            </div>
          )}
        </CardHeader>
        {ca && !ca.trusted && (
          <CardContent>
            <p className="rounded-lg border border-warn/25 bg-warn/10 px-3 py-2 text-[11.5px] text-warn">
              {/Mac/i.test(typeof navigator !== "undefined" ? navigator.userAgent : "")
                ? t("tls.trustMacHint")
                : t("tls.trustWinHint")}
            </p>
          </CardContent>
        )}
      </Card>
      {trustError && <CertError error={trustError} />}

      {/* 站点证书 */}
      <h2 className="mb-3 text-[15px] font-semibold">{t("tls.certs")}</h2>
      {siteCerts.length === 0 && ready ? (
        <EmptyState icon={ShieldCheck} title={t("tls.empty")} hint={t("tls.issueHint")} />
      ) : (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
          {siteCerts.map((c) => (
            <Card key={c.id} className="p-4">
              <div className="flex flex-wrap items-start justify-between gap-2">
                <div className="min-w-0">
                  <p className="truncate font-mono text-[13px] font-medium">{c.subject}</p>
                  <p className="mt-0.5 text-[11px] text-faint">
                    {c.sans.length > 1 ? `${c.sans.length} ${t("tls.domainsCount")}` : t("tls.singleDomain")} ·{" "}
                    {formatDate(c.notBefore)} →{" "}
                    {formatDate(c.notAfter)}
                  </p>
                </div>
                <Badge variant={daysLeft(c.notAfter) < 7 ? "error" : daysLeft(c.notAfter) < 30 ? "warn" : "running"}>
                  <CalendarClock className="h-3 w-3" /> {c.notAfter <= Date.now() ? t("cert.expired") : `${daysLeft(c.notAfter)} ${t("tls.daysLeft")}`}
                </Badge>
              </div>
              <div className="mt-3 flex flex-wrap gap-2 border-t border-dashed border-separator pt-3">
                {c.kind === "site" && (
                  <Button variant="secondary" size="sm" disabled={!ready || reissuing} onClick={(event) => { actionTrigger.current = event.currentTarget; setActionError(null); setReissueTarget({ cert: c, action: "reissue" }); }}>
                    <RefreshCw className="h-3 w-3" /> {t("tls.reissue")}
                  </Button>
                )}
                <Button variant="secondary" size="sm" disabled={!ready || reissuing} onClick={(event) => { actionTrigger.current = event.currentTarget; setPfxTarget(c); }}>
                  <Download className="h-3 w-3" /> {t("pfx.export")}
                </Button>
                {c.kind === "site" ? <Button
                  variant="ghost"
                  size="sm"
                  className="text-error hover:text-error"
                  disabled={!ready || reissuing}
                  onClick={(event) => { actionTrigger.current = event.currentTarget; setActionError(null); setReissueTarget({ cert: c, action: "delete" }); }}
                >
                  <Trash2 className="h-3 w-3" /> {t("tls.deleteLocal")}
                </Button> : <Button variant="ghost" size="sm" onClick={() => setTab("automation")}>{t("tls.tab.automation")}</Button>}
              </div>
            </Card>
          ))}
        </div>
      )}

      <IssueCertDialog open={issueOpen} onOpenChange={setIssueOpen} onDone={refresh} onCloseAutoFocus={restoreActionFocus} />

      {/* 重新签发替换本地文件；删除须先解除站点引用。 */}
      <ConfirmDialog
        onCloseAutoFocus={restoreActionFocus}
        open={reissueTarget !== null}
        onOpenChange={(o) => !o && !actionRef.current && setReissueTarget(null)}
        title={`${t(reissueTarget?.action === "delete" ? "tls.deleteLocal" : "confirm.reissueCerts")} · ${reissueTarget?.cert.subject ?? ""}`}
        description={t(reissueTarget?.action === "delete" ? "tls.deleteLocalHint" : "confirm.reissueCertsDesc").replace("{name}", reissueTarget?.cert.subject ?? "")}
        confirmText={t(reissueTarget?.action === "delete" ? "tls.deleteLocal" : "tls.reissue")}
        danger={reissueTarget?.action === "delete"}
        loading={reissuing}
        confirmDisabled={!ready}
        onConfirm={async () => {
          if (!reissueTarget || actionRef.current || !ready) return;
          actionRef.current = true;
          setReissuing(true);
          setActionError(null);
          let completed = false;
          try {
            if (reissueTarget.action === "delete") {
              await api.deleteLocalCert(reissueTarget.cert.id);
              toast.success(t("cert.deleted"));
            } else {
              await api.issueCert(reissueTarget.cert.subject, reissueTarget.cert.sans);
              toast.success(`${t("tls.reissuedP1")} ${reissueTarget.cert.subject} ${t("tls.reissuedP2")}`);
            }
            completed = true;
          } catch (e) {
            setActionError(normalizeError(e));
          } finally {
            await refresh();
            actionRef.current = false;
            setReissuing(false);
            if (completed) setReissueTarget(null);
          }
        }}
      >{actionError && <CertError error={actionError} />}</ConfirmDialog>
          <PfxExportDialog cert={pfxTarget} onClose={() => setPfxTarget(null)} onCloseAutoFocus={restoreActionFocus} />
          </div>
        </TabsContent>

        {/* ============ 自动签发：ACME 签发 + 定时续签 + 多平台部署（certd 式） ============ */}
        <TabsContent value="automation" className="mt-0">
          <CertAutomationSection />
        </TabsContent>
      </Tabs>
    </div>
  );
}

function CertError({ error }: { error: AppErrorShape }) {
  return <div role="alert" className="rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">
    <p>{error.message}</p>{error.hint && <p className="mt-1">{error.hint}</p>}
  </div>;
}

function IssueCertDialog({ open, onOpenChange, onDone, onCloseAutoFocus }: { open: boolean; onOpenChange: (o: boolean) => void; onDone: () => Promise<unknown>; onCloseAutoFocus: (event: Event) => void }) {
  const t = useT();
  const [domain, setDomain] = React.useState("");
  const [sans, setSans] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const submit = async () => {
    if (busyRef.current || !domain.trim()) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const extra = sans.split(/[,，\s]+/).filter(Boolean);
      await api.issueCert(domain.trim(), extra);
      toast.success(`${t("tls.issuedP1")}${domain}`);
      onOpenChange(false);
      setDomain("");
      setSans("");
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      await onDone();
      busyRef.current = false;
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(next) => { if (!busyRef.current) onOpenChange(next); }}>
      <DialogContent hideClose={busy} onCloseAutoFocus={onCloseAutoFocus} className="flex max-h-[85dvh] max-w-md flex-col overflow-hidden">
        <div className="min-h-0 space-y-4 overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="pr-6 leading-snug">{t("tls.issueTitle")}</DialogTitle>
          <DialogDescription>{t("tls.certHint")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="cert-primary">{t("tls.primaryDomain")}</Label>
            <Input id="cert-primary" disabled={busy} value={domain} onChange={(e) => { setDomain(e.target.value); setError(null); }} placeholder="myapp.test" className="font-mono" autoFocus />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="cert-sans">{t("tls.extraDomains")}</Label>
            <Input id="cert-sans" disabled={busy} value={sans} onChange={(e) => { setSans(e.target.value); setError(null); }} placeholder={t("tls.sansPlaceholder")} className="font-mono" />
            <p className="text-xs text-muted">{t("tls.domainHint")}</p>
          </div>
        </div>
        {error && <CertError error={error} />}
        </div>
        <DialogFooter className="shrink-0">
          <Button variant="ghost" disabled={busy} onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={!domain.trim() || busy}>{t(busy ? "confirm.busy" : "tls.issue")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}


/* ============ PFX 导出（Windows IIS / 设备导入） ============ */
function PfxExportDialog({ cert, onClose, onCloseAutoFocus }: { cert: CertRecord | null; onClose: () => void; onCloseAutoFocus: (event: Event) => void }) {
  const t = useT();
  const [password, setPassword] = React.useState("");
  const [format, setFormat] = React.useState<"pfx" | "der" | "jks" | "pem">("pfx");
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  React.useEffect(() => { setPassword(""); setError(null); }, [cert?.id]);

  const doExport = async () => {
    if (!cert || busyRef.current) return;
    if (!isTauri) { setError({ code: "DESKTOP_ONLY", message: t("tls.desktopOnly") }); return; }
    if (format === "jks" && [...password].length < 6) {
      setError({ code: "JKS_PASSWORD", message: t("pfx.jksMin") }); return;
    }
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const ext = format === "pfx" ? "pfx" : format === "jks" ? "jks" : format === "pem" ? "pem" : "der";
      const path = await save({
        title: t("pfx.saveTitle"),
        defaultPath: `${cert.subject.replace(/[^a-zA-Z0-9.-]/g, "_")}.${ext}`,
        filters: [
          format === "pfx"
            ? { name: "PKCS#12", extensions: ["pfx", "p12"] }
            : format === "jks"
              ? { name: "Java Keystore", extensions: ["jks"] }
              : format === "pem"
                ? { name: "PEM 打包", extensions: ["pem"] }
                : { name: "DER 证书", extensions: ["der", "crt"] },
        ],
      });
      if (!path || typeof path !== "string") {
        return;
      }
      const out =
        format === "pfx"
          ? await api.certExportPfx(cert.id, password, path)
          : format === "jks"
            ? await api.certExportJks(cert.id, password, path)
            : format === "pem"
              ? await api.certExportPem(cert.id, path)
              : await api.certExportDer(cert.id, path);
      toast.success(t("pfx.exported"), { description: out });
      onClose();
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  return (
    <Dialog open={cert !== null} onOpenChange={(o) => { if (!o && !busyRef.current) onClose(); }}>
      <DialogContent hideClose={busy} onCloseAutoFocus={onCloseAutoFocus} className="flex max-h-[85dvh] max-w-sm flex-col overflow-hidden">
        <div className="min-h-0 space-y-4 overflow-y-auto [overflow-wrap:anywhere]">
        <DialogHeader>
          <DialogTitle className="pr-6 leading-snug">{t("pfx.export")} · {cert?.subject ?? ""}</DialogTitle>
          <DialogDescription>{t(`pfx.format.${format}`)}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-1.5">
          <Label id="cert-format-label">{t("pfx.format")}</Label>
          <div role="group" aria-labelledby="cert-format-label" className="grid grid-cols-2 gap-1 rounded-lg bg-card-2/60 p-1">
            {(["pfx", "jks", "pem", "der"] as const).map((f) => (
              <button
                key={f}
                type="button"
                disabled={busy}
                aria-pressed={format === f}
                onClick={() => { setFormat(f); setError(null); }}
                className={cn(
                  "flex-1 rounded-md px-2 py-1 text-[11.5px] font-medium transition-all",
                  format === f ? "bg-surface text-foreground shadow-sm" : "text-faint hover:text-secondary"
                )}
              >
                {f.toUpperCase()}
              </button>
            ))}
          </div>
        </div>
        {format === "pfx" || format === "jks" ? (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pfx-pass">
            {t("pfx.password")}
            {format === "jks" ? ` · ${t("pfx.jksMin")}` : ""}
          </Label>
          <Input
            id="pfx-pass"
            type="password"
            disabled={busy}
            value={password}
            onChange={(e) => { setPassword(e.target.value); setError(null); }}
            placeholder={t(format === "jks" ? "pfx.jksMin" : "pfx.passwordPlaceholder")}
            className="font-mono text-[12px]"
            autoFocus
          />
        </div>
        ) : null}
        {!isTauri && <p className="text-xs text-muted">{t("tls.desktopOnly")}</p>}
        {error && <CertError error={error} />}
        </div>
        <DialogFooter className="shrink-0">
          <Button variant="ghost" onClick={onClose} disabled={busy}>{t("common.cancel")}</Button>
          <Button onClick={doExport} disabled={busy || !isTauri || (format === "jks" && [...password].length < 6)}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <KeyRound className="h-3.5 w-3.5" />}
            {t("pfx.export")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
