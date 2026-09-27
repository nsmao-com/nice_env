"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  ShieldCheck,
  ShieldAlert,
  ShieldX,
  ShieldQuestion,
  FileKey2,
  Upload,
  Trash2,
  RefreshCw,
  Loader2,
  CalendarClock,
  AlertTriangle,
} from "lucide-react";
import type { CertReport, ImportedCert } from "@nsb/schema";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/shared/misc";

/**
 * 证书体检。
 *
 * 本地签发和用户导入的证书都有有效期，过期后站点连接会被浏览器拦截，
 * 浏览器给的原因往往看不出是证书过期。这里把剩余天数、文件是否还在、
 * 站点域名有没有被 SAN 覆盖一次列出来。
 */
export function CertHealthCard() {
  const t = useT();
  const qc = useQueryClient();
  const reportQuery = useQuery({ queryKey: ["cert-health"], queryFn: api.certHealth });
  const importedQuery = useQuery({ queryKey: ["cert-imported"], queryFn: api.certImportedList });
  const report = reportQuery.data;
  const imported = importedQuery.data ?? [];
  const loading = reportQuery.isFetching || importedQuery.isFetching;
  const readError = reportQuery.error || importedQuery.error;
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [confirmDel, setConfirmDel] = React.useState<ImportedCert | null>(null);
  const [replacement, setReplacement] = React.useState<{ cert: ImportedCert; certPath: string; keyPath: string } | null>(null);

  const load = () => Promise.all([
    reportQuery.refetch(), importedQuery.refetch(),
    qc.invalidateQueries({ queryKey: ["site-certificate-choices"] }),
  ]);

  const pickReplacement = async (field: "certPath" | "keyPath") => {
    if (busyRef.current) return;
    if (!isTauri) { toast.info(t("tls.desktopOnly")); return; }
    busyRef.current = true;
    setBusy(true);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ title: t(field === "certPath" ? "cert.pickCert" : "cert.pickKey"), multiple: false,
        filters: [{ name: "PEM", extensions: field === "certPath" ? ["crt", "pem", "cer"] : ["key", "pem"] }] });
      if (typeof path === "string") {
        setReplacement((current) => current ? { ...current, [field]: path } : current);
        setError(null);
      }
    } catch (e) { setError(normalizeError(e)); }
    finally { busyRef.current = false; setBusy(false); }
  };

  const doReplace = async () => {
    if (busyRef.current || !replacement?.certPath || !replacement.keyPath) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      await api.certImportedReplace(replacement.cert.id, replacement.certPath, replacement.keyPath);
      toast.success(t("cert.replaced"));
      setReplacement(null);
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      // 重载失败也可能已保存新材料；重新读取真实状态，不保留旧有效期。
      await load();
      await qc.invalidateQueries({ queryKey: ["services"] });
      busyRef.current = false;
      setBusy(false);
    }
  };

  const doImport = async () => {
    if (busyRef.current) return;
    if (!isTauri) { toast.info(t("tls.desktopOnly")); return; }
    busyRef.current = true;
    setBusy(true);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const certPath = await open({
        title: t("cert.pickCert"),
        multiple: false,
        filters: [{ name: "Certificate", extensions: ["crt", "pem", "cer"] }],
      });
      if (typeof certPath !== "string") return;
      const keyPath = await open({
        title: t("cert.pickKey"),
        multiple: false,
        filters: [{ name: "Private key", extensions: ["key", "pem"] }],
      });
      if (typeof keyPath !== "string") return;
      const r = await api.certImport(certPath, keyPath);
      toast.success(t("cert.imported"), {
        description: `${r.subject} · ${t("cert.daysLeft").replace("{n}", String(r.daysLeft))}`,
      });
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  const doDelete = async (c: ImportedCert) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      await api.certImportedDelete(c.certPath);
      toast.success(t("cert.deleted"));
      setConfirmDel(null);
      await load();
    } catch (e) {
      setError(normalizeError(e));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  const issues = report?.certs.filter((c) => c.filePresent === false || c.advice !== "") ?? [];
  const healthy = (report?.certs.length ?? 0) - issues.length;

  return (
    <>
      <Card>
        <CardHeader className="flex-row flex-wrap items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-fill">
            <ShieldCheck className="h-4 w-4 text-primary" strokeWidth={1.8} />
          </div>
          <div className="min-w-0 flex-1">
            <CardTitle className="text-[13px]">{t("cert.title")}</CardTitle>
            <CardDescription className="text-[11px]">
              {loading
                ? t("common.loading")
                : readError ? t("tls.readFailed") : report
                  ? `${healthy} ${t("cert.healthy")} · ${issues.length} ${t("cert.needAttention")}`
                  : t("cert.subtitle")}
            </CardDescription>
          </div>
          <Button size="sm" variant="ghost" className="h-8" aria-label={t("bulk.retry")} onClick={() => void load()} disabled={loading || busy}>
            <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
          </Button>
          <Button size="sm" variant="secondary" className="h-8" onClick={() => void doImport()} disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Upload className="h-3.5 w-3.5" />}
            <span className="ml-1.5">{t("cert.import")}</span>
          </Button>
        </CardHeader>
        <CardContent>
          {readError && <p role="alert" className="mb-3 rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">{t("tls.readFailed")} {normalizeError(readError).message}</p>}
          {/* 需要处理的问题，排最前 */}
          {issues.length > 0 && (
            <div className="mb-3 space-y-1.5">
              {issues.slice(0, 5).map((c) => (
                <CertRow key={c.id} cert={c} />
              ))}
            </div>
          )}

          {/* 健康的折叠成一行摘要，不占地方 */}
          {!readError && issues.length === 0 && report && report.certs.length > 0 && (
            <p className="py-3 text-center text-[12px] text-running">{t("cert.allGood")}</p>
          )}

          {report && !readError && !report.caTrusted && report.certs.some((c) => c.kind === "ca" && c.filePresent && !["invalid", "expired"].includes(c.status)) && (
            <div className="mt-2 flex items-start gap-2 rounded-lg border border-warn/25 bg-warn-soft px-2.5 py-2">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
              <span className="text-[11.5px]">{t("cert.caNotTrusted")}</span>
            </div>
          )}

          {/* 导入的证书 */}
          {imported.length > 0 && (
            <div className="mt-4">
              <div className="mb-2 flex items-center gap-2">
                <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                  {t("cert.importedList")}
                </span>
                <span className="mx-2 flex-1 border-t border-dashed border-border/60" />
              </div>
              <div className="space-y-1.5">
                {imported.map((c) => (
                  <div
                    key={c.certPath}
                    className="group min-w-0 rounded-lg border border-border/60 bg-card-2/25 px-3 py-3"
                  >
                    <div className="flex min-w-0 items-start gap-2.5">
                      <FileKey2 className="mt-0.5 h-3.5 w-3.5 shrink-0 text-faint" />
                      <div className="min-w-0 flex-1">
                        <span className="block font-mono text-[11.5px] [overflow-wrap:anywhere]">{c.subject}</span>
                        <p className="mt-1 text-[10.5px] text-faint [overflow-wrap:anywhere]">{c.sans.join(", ")}</p>
                      </div>
                    </div>
                    <div className="mt-2 flex flex-wrap items-center gap-2">
                      <Badge
                        variant="outline"
                        className={cn(
                          "shrink-0 text-[9.5px]",
                          !c.usable || c.daysLeft <= 7
                            ? "text-error"
                            : c.daysLeft <= 30
                              ? "text-warn"
                              : ""
                        )}
                      >
                        {!c.usable
                          ? t("cert.invalid")
                          : c.daysLeft < 0
                            ? t("cert.expired")
                            : t("cert.daysLeft").replace("{n}", String(c.daysLeft))}
                      </Badge>
                    </div>
                    {!c.usable && c.problem && <p className="mt-2 text-[11px] text-error [overflow-wrap:anywhere]">{c.problem}</p>}
                    {c.usedBySites.length > 0 && <p className="mt-2 text-[11px] text-muted [overflow-wrap:anywhere]">{t("cert.usedBy")}: {c.usedBySites.join(", ")}</p>}
                    <div className="mx-2 my-3 border-t border-dashed border-border/60" />
                    <div className="flex flex-wrap items-center gap-2">
                      <Button variant="secondary" size="sm" disabled={busy || !!readError}
                        onClick={() => { setError(null); setReplacement({ cert: c, certPath: "", keyPath: "" }); }}
                        aria-label={`${t("cert.replace")} ${c.subject}`}>
                        <RefreshCw className="mr-1.5 h-3.5 w-3.5" />{t("cert.replace")}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="shrink-0 px-2 text-error"
                        disabled={busy || !!readError}
                        onClick={() => { setError(null); setConfirmDel(c); }}
                        aria-label={`${t("cert.delete")} ${c.subject}`}
                        title={t("cert.delete")}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      <ConfirmDialog
        open={replacement != null}
        onOpenChange={(open) => { if (!open && !busyRef.current) { setReplacement(null); setError(null); } }}
        title={t("cert.replace")}
        description={t("cert.replaceHint")}
        confirmText={t("cert.replaceApply")}
        loading={busy}
        confirmDisabled={!replacement?.certPath || !replacement?.keyPath}
        onConfirm={() => void doReplace()}
      >
        <p className="text-sm font-medium [overflow-wrap:anywhere]">{replacement?.cert.subject}</p>
        {!!replacement?.cert.usedBySites.length && <p className="text-xs leading-relaxed text-muted [overflow-wrap:anywhere]">
          {t("cert.usedBy")}: {replacement.cert.usedBySites.join(", ")}
        </p>}
        {(["certPath", "keyPath"] as const).map((field) => <div key={field} className="space-y-2">
          <Button variant="secondary" className="w-full justify-start whitespace-normal text-left" disabled={busy} onClick={() => void pickReplacement(field)}>
            <Upload className="mr-2 h-3.5 w-3.5 shrink-0" />{t(field === "certPath" ? "cert.pickCert" : "cert.pickKey")}
          </Button>
          {replacement?.[field] && <p className="text-xs text-muted [overflow-wrap:anywhere]">{replacement[field]}</p>}
        </div>)}
        {error && <p role="alert" className="rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">
          {error.message}{error.hint && <span className="mt-1 block">{error.hint}</span>}
        </p>}
      </ConfirmDialog>

      <ConfirmDialog
        open={confirmDel != null}
        onOpenChange={(v) => !v && !busyRef.current && setConfirmDel(null)}
        title={t("cert.deleteTitle")}
        description={t("cert.deleteDesc").replace("{n}", confirmDel?.subject ?? "")}
        confirmText={t("cert.delete")}
        danger
        loading={busy}
        onConfirm={() => {
          const c = confirmDel;
          if (c) void doDelete(c);
        }}
      >{error && <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{error.message}{error.hint && <span className="mt-1 block">{error.hint}</span>}</p>}</ConfirmDialog>
    </>
  );
}

function CertRow({ cert }: { cert: CertReport["certs"][number] }) {
  const t = useT();
  const Icon =
    cert.status === "expired" || cert.status === "invalid"
      ? ShieldX
      : cert.status === "critical"
        ? ShieldAlert
        : cert.status === "warn"
          ? ShieldAlert
          : cert.filePresent
            ? ShieldCheck
            : ShieldQuestion;
  const tone =
    cert.status === "expired" || cert.status === "invalid" || cert.status === "critical" || !cert.filePresent
      ? "text-error"
      : cert.status === "warn"
        ? "text-warn"
        : "text-running";
  return (
    <div className="flex items-start gap-2.5 rounded-lg border border-border/60 bg-card-2/25 px-2.5 py-2">
      <Icon className={cn("mt-0.5 h-3.5 w-3.5 shrink-0", tone)} strokeWidth={2} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="truncate font-mono text-[11.5px]">{cert.subject}</span>
          <Badge variant="outline" className="shrink-0 text-[9.5px]">
            {cert.kind === "ca" ? "CA" : cert.kind === "acme" ? "ACME" : cert.kind === "imported" ? t("sites.detail.importedCerts") : "site"}
          </Badge>
          <span className={cn("shrink-0 text-[10.5px]", tone)}>
            <CalendarClock className="mr-0.5 inline h-3 w-3" />
            {!cert.filePresent || cert.notAfter <= 0 ? t("tls.statusUnknown") : cert.status === "invalid" ? t("cert.invalid") : cert.daysLeft < 0
              ? t("cert.expired")
              : t("cert.daysLeft").replace("{n}", String(cert.daysLeft))}
          </span>
        </div>
        {cert.advice && <p className="mt-0.5 text-[11px] text-muted [overflow-wrap:anywhere]">{cert.advice}</p>}
        {cert.usedBySites.length > 0 && (
          <p className="mt-0.5 text-[10.5px] text-faint">
            {t("cert.usedBy")}: {cert.usedBySites.join(", ")}
          </p>
        )}
      </div>
    </div>
  );
}
