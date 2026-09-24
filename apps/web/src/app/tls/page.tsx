"use client";

import * as React from "react";
import { toast } from "sonner";
import { ShieldCheck, ShieldX, RefreshCw, Plus, Ban, CalendarClock } from "lucide-react";
import type { CertRecord } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useCerts, useInvalidate, toastError } from "@/lib/hooks";
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
  const { data: certs } = useCerts();
  const invalidate = useInvalidate();
  const [issueOpen, setIssueOpen] = React.useState(false);
  const [reissueTarget, setReissueTarget] = React.useState<CertRecord | null>(null);
  const [reissuing, setReissuing] = React.useState(false);
  const [pfxTarget, setPfxTarget] = React.useState<CertRecord | null>(null);
  const ca = certs.find((c) => c.kind === "ca");
  // 本机证书 = 自签(site) + ACME 签发(acme) 都算；CA 行单独展示
  const siteCerts = certs.filter((c) => c.kind === "site" || c.kind === "acme");

  const daysLeft = (ts: number) => Math.max(0, Math.round((ts - Date.now()) / 86400_000));

  /** 从文件夹批量导入证书文件（certd 没导出配置、只剩证书文件时的迁移路） */
  const importCertDir = async () => {
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
        invalidate("certs");
      } else {
        toast.warning(t("tls.importDirNone"), { description: desc || t("tls.importDirNoneHint"), duration: 12000 });
      }
    } catch (e) {
      toastError(e, t("tls.importDirFailed"));
    }
  };

  return (
    <div className="pb-8">
      <PageHeader
        title={t("tls.title")}
        subtitle={t("tls.subtitle")}
        actions={
          <>
            <Button variant="secondary" onClick={importCertDir}>
              <FolderArchive className="h-3.5 w-3.5" /> {t("tls.importDir")}
            </Button>
            <Button onClick={() => setIssueOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("tls.issueTitle")}
            </Button>
          </>
        }
      />

      <Tabs defaultValue="local">
        <TabsList className="mb-5">
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
      <Card className="mb-6">
        <CardHeader className="flex-row items-center justify-between">
          <div className="flex items-center gap-3">
            <div className={`flex h-10 w-10 items-center justify-center rounded-xl border ${ca?.trusted ? "border-running/30 bg-running-soft" : "border-warn/30 bg-warn/10"}`}>
              {ca?.trusted ? (
                <ShieldCheck className="h-5 w-5 text-running" strokeWidth={1.8} />
              ) : (
                <ShieldX className="h-5 w-5 text-warn" strokeWidth={1.8} />
              )}
            </div>
            <div>
              <CardTitle className="text-[14px]">{t("tls.ca")}</CardTitle>
              <CardDescription className="mt-1 font-mono text-[11px]">
                {ca ? `CN=${ca.subject} · ${t("dash.tenYears")}` : t("tls.caNotCreated")}
              </CardDescription>
            </div>
          </div>
          {ca && (
            <div className="flex items-center gap-2">
              <Badge variant={ca.trusted ? "running" : "warn"}>
                {ca.trusted ? t("tls.trusted") : t("tls.notTrusted")}
              </Badge>
              {!ca.trusted && (
                <Button
                  onClick={async () => {
                    try {
                      await api.trustCa();
                      toast.success(t("tls.caTrusted"));
                      invalidate("certs");
                    } catch (e) {
                      toastError(e, t("tls.trustFailed"));
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

      {/* 站点证书 */}
      <h2 className="mb-3 text-[15px] font-semibold">{t("tls.certs")}</h2>
      {siteCerts.length === 0 ? (
        <EmptyState icon={ShieldCheck} title={t("tls.empty")} hint={t("tls.issueHint")} />
      ) : (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
          {siteCerts.map((c) => (
            <Card key={c.id} className="p-4">
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <p className="truncate font-mono text-[13px] font-medium">{c.subject}</p>
                  <p className="mt-0.5 text-[11px] text-faint">
                    {c.sans.length > 1 ? `${c.sans.length} ${t("tls.domainsCount")}` : t("tls.singleDomain")} ·{" "}
                    {new Date(c.notBefore).toLocaleDateString("zh-CN")} →{" "}
                    {new Date(c.notAfter).toLocaleDateString("zh-CN")}
                  </p>
                </div>
                <Badge variant={daysLeft(c.notAfter) < 7 ? "error" : daysLeft(c.notAfter) < 30 ? "warn" : "running"}>
                  <CalendarClock className="h-3 w-3" /> {daysLeft(c.notAfter)} {t("tls.daysLeft")}
                </Badge>
              </div>
              <div className="mt-3 flex gap-2 border-t border-border pt-3">
                {c.kind === "site" && (
                  <Button variant="secondary" size="sm" onClick={() => setReissueTarget(c)}>
                    <RefreshCw className="h-3 w-3" /> {t("tls.reissue")}
                  </Button>
                )}
                <Button variant="secondary" size="sm" onClick={() => setPfxTarget(c)}>
                  <Download className="h-3 w-3" /> {t("pfx.export")}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  className="text-error hover:text-error"
                  onClick={() =>
                    toast.info(t("tls.revoke"), {
                      description: t("tls.revokeHint"),
                    })
                  }
                >
                  <Ban className="h-3 w-3" /> {t("tls.revoke")}
                </Button>
              </div>
            </Card>
          ))}
        </div>
      )}

      <IssueCertDialog open={issueOpen} onOpenChange={setIssueOpen} onDone={() => invalidate("certs")} />

      {/* 重新签发会替换旧证书，先确认（旧证书立即失效） */}
      <ConfirmDialog
        open={reissueTarget !== null}
        onOpenChange={(o) => !o && setReissueTarget(null)}
        title={`${t("confirm.reissueCerts")} · ${reissueTarget?.subject ?? ""}`}
        description={t("confirm.reissueCertsDesc").replace("{name}", reissueTarget?.subject ?? "")}
        confirmText={t("tls.reissue")}
        loading={reissuing}
        onConfirm={async () => {
          if (!reissueTarget) return;
          setReissuing(true);
          try {
            await api.issueCert(reissueTarget.subject, reissueTarget.sans);
            toast.success(`${t("tls.reissuedP1")} ${reissueTarget.subject} ${t("tls.reissuedP2")}`);
            invalidate("certs");
          } catch (e) {
            toastError(e);
          } finally {
            setReissuing(false);
            setReissueTarget(null);
          }
        }}
      />
          <PfxExportDialog cert={pfxTarget} onClose={() => setPfxTarget(null)} />
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

function IssueCertDialog({ open, onOpenChange, onDone }: { open: boolean; onOpenChange: (o: boolean) => void; onDone: () => void }) {
  const t = useT();
  const [domain, setDomain] = React.useState("");
  const [sans, setSans] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      const extra = sans.split(/[,，\s]+/).filter(Boolean);
      await api.issueCert(domain, [domain, ...extra]);
      toast.success(`${t("tls.issuedP1")}${domain}`);
      onOpenChange(false);
      setDomain("");
      setSans("");
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t("tls.issueTitle")}</DialogTitle>
          <DialogDescription>{t("tls.certHint")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Input value={domain} onChange={(e) => setDomain(e.target.value)} placeholder="myapp.test" className="font-mono" autoFocus />
          </div>
          <Input value={sans} onChange={(e) => setSans(e.target.value)} placeholder={t("tls.sansPlaceholder")} className="font-mono" />
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={!domain || busy}>{t("tls.issue")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}


/* ============ PFX 导出（Windows IIS / 设备导入） ============ */
function PfxExportDialog({ cert, onClose }: { cert: CertRecord | null; onClose: () => void }) {
  const t = useT();
  const [password, setPassword] = React.useState("");
  const [format, setFormat] = React.useState<"pfx" | "der" | "jks" | "pem">("pfx");
  const [busy, setBusy] = React.useState(false);

  const doExport = async () => {
    if (!cert) return;
    setBusy(true);
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const ext = format === "pfx" ? "pfx" : format === "jks" ? "jks" : format === "pem" ? "pem" : "der";
      const path = await save({
        title: t("pfx.saveTitle"),
        defaultPath: `${cert.subject}.${ext}`,
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
        setBusy(false);
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
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={cert !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle>{t("pfx.export")} · {cert?.subject ?? ""}</DialogTitle>
          <DialogDescription>{t("pfx.hint")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-1.5">
          <Label>{t("pfx.format")}</Label>
          <div className="flex gap-1 rounded-lg bg-card-2/60 p-1">
            {(["pfx", "jks", "pem", "der"] as const).map((f) => (
              <button
                key={f}
                type="button"
                onClick={() => setFormat(f)}
                className={cn(
                  "flex-1 rounded-md px-2 py-1 text-[11.5px] font-medium transition-all",
                  format === f ? "bg-surface text-foreground shadow-sm" : "text-faint hover:text-secondary"
                )}
              >
                {t(`pfx.format.${f}`)}
              </button>
            ))}
          </div>
        </div>
        {format === "pfx" || format === "jks" ? (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pfx-pass">
            {t("pfx.password")}
            {format === "jks" ? `（${t("pfx.jksMin")}）` : ""}
          </Label>
          <Input
            id="pfx-pass"
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={t("pfx.passwordPlaceholder")}
            className="font-mono text-[12px]"
            autoFocus
          />
        </div>
        ) : null}
        <DialogFooter>
          <Button variant="ghost" onClick={onClose} disabled={busy}>{t("common.cancel")}</Button>
          <Button onClick={doExport} disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <KeyRound className="h-3.5 w-3.5" />}
            {t("pfx.export")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
