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

export default function TlsPage() {
  const t = useT();
  const { data: certs } = useCerts();
  const invalidate = useInvalidate();
  const [issueOpen, setIssueOpen] = React.useState(false);
  const [reissueTarget, setReissueTarget] = React.useState<CertRecord | null>(null);
  const [reissuing, setReissuing] = React.useState(false);
  const ca = certs.find((c) => c.kind === "ca");
  const siteCerts = certs.filter((c) => c.kind === "site");

  const daysLeft = (ts: number) => Math.max(0, Math.round((ts - Date.now()) / 86400_000));

  return (
    <div className="pb-8">
      <PageHeader
        title={t("tls.title")}
        subtitle={t("tls.subtitle")}
        actions={
          <Button onClick={() => setIssueOpen(true)}>
            <Plus className="h-3.5 w-3.5" /> {t("tls.issueTitle")}
          </Button>
        }
      />

      {/* 证书体检：放在页头下方，不要塞进 actions —— 那会变成 button 套 button */}
      <section className="mb-6">
        <CertHealthCard />
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
                {ca ? `CN=NiceServBay Local Root CA · ${t("dash.tenYears")}` : t("tls.caNotCreated")}
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
                <Button variant="secondary" size="sm" onClick={() => setReissueTarget(c)}>
                  <RefreshCw className="h-3 w-3" /> {t("tls.reissue")}
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
