"use client";

import * as React from "react";
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
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/shared/misc";

/**
 * 证书体检。
 *
 * 自签证书不会过期，但「用户导入的证书」有真实有效期，过期当天站点直接打不开，
 * 浏览器给的原因往往看不出是证书过期。这里把剩余天数、文件是否还在、
 * 站点域名有没有被 SAN 覆盖一次列出来。
 */
export function CertHealthCard() {
  const t = useT();
  const [report, setReport] = React.useState<CertReport | null>(null);
  const [imported, setImported] = React.useState<ImportedCert[]>([]);
  const [loading, setLoading] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [confirmDel, setConfirmDel] = React.useState<ImportedCert | null>(null);

  const load = React.useCallback(async () => {
    setLoading(true);
    try {
      const [r, im] = await Promise.all([
        api.certHealth(),
        api.certImportedList().catch(() => []),
      ]);
      setReport(r);
      setImported(im);
    } catch {
      /* 无证书时不报错 */
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  const doImport = async () => {
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
      setBusy(true);
      const r = await api.certImport(certPath, keyPath);
      toast.success(t("cert.imported"), {
        description: `${r.subject} · ${t("cert.daysLeft").replace("{n}", String(r.daysLeft))}`,
      });
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const doDelete = async (c: ImportedCert) => {
    try {
      await api.certImportedDelete(c.certPath);
      toast.success(t("cert.deleted"));
      await load();
    } catch (e) {
      toastError(e);
    }
  };

  const issues = report?.certs.filter((c) => c.filePresent === false || c.advice !== "") ?? [];
  const healthy = (report?.certs.length ?? 0) - issues.length;

  return (
    <>
      <Card>
        <CardHeader className="flex-row items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-fill">
            <ShieldCheck className="h-4 w-4 text-primary" strokeWidth={1.8} />
          </div>
          <div className="min-w-0 flex-1">
            <CardTitle className="text-[13px]">{t("cert.title")}</CardTitle>
            <CardDescription className="text-[11px]">
              {loading
                ? t("common.loading")
                : report
                  ? `${healthy} ${t("cert.healthy")} · ${issues.length} ${t("cert.needAttention")}`
                  : t("cert.subtitle")}
            </CardDescription>
          </div>
          <Button size="sm" variant="ghost" className="h-8" onClick={() => void load()} disabled={loading}>
            <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
          </Button>
          <Button size="sm" variant="secondary" className="h-8" onClick={() => void doImport()} disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Upload className="h-3.5 w-3.5" />}
            <span className="ml-1.5">{t("cert.import")}</span>
          </Button>
        </CardHeader>
        <CardContent>
          {/* 需要处理的问题，排最前 */}
          {issues.length > 0 && (
            <div className="mb-3 space-y-1.5">
              {issues.slice(0, 5).map((c) => (
                <CertRow key={c.id} cert={c} />
              ))}
            </div>
          )}

          {/* 健康的折叠成一行摘要，不占地方 */}
          {issues.length === 0 && report && report.certs.length > 0 && (
            <p className="py-3 text-center text-[12px] text-running">{t("cert.allGood")}</p>
          )}

          {report && !report.caTrusted && (
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
                <span className="h-px flex-1 bg-border/60" />
              </div>
              <div className="space-y-1.5">
                {imported.map((c) => (
                  <div
                    key={c.certPath}
                    className="group flex items-center gap-2.5 rounded-lg border border-border/60 bg-card-2/25 px-2.5 py-2"
                  >
                    <FileKey2 className="h-3.5 w-3.5 shrink-0 text-faint" />
                    <div className="min-w-0 flex-1">
                      <span className="truncate font-mono text-[11.5px]">{c.subject}</span>
                      <p className="truncate text-[10.5px] text-faint">
                        {c.sans.slice(0, 3).join(", ")}
                        {c.sans.length > 3 ? ` +${c.sans.length - 3}` : ""}
                      </p>
                    </div>
                    <Badge
                      variant="outline"
                      className={cn(
                        "shrink-0 text-[9.5px]",
                        c.daysLeft < 0
                          ? "text-error"
                          : c.daysLeft <= 7
                            ? "text-error"
                            : c.daysLeft <= 30
                              ? "text-warn"
                              : ""
                      )}
                    >
                      {c.daysLeft < 0
                        ? t("cert.expired")
                        : t("cert.daysLeft").replace("{n}", String(c.daysLeft))}
                    </Badge>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 shrink-0 px-2 text-error opacity-0 transition-opacity group-hover:opacity-100"
                      onClick={() => setConfirmDel(c)}
                      title={t("cert.delete")}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                ))}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      <ConfirmDialog
        open={confirmDel != null}
        onOpenChange={(v) => !v && setConfirmDel(null)}
        title={t("cert.deleteTitle")}
        description={t("cert.deleteDesc").replace("{n}", confirmDel?.subject ?? "")}
        confirmText={t("cert.delete")}
        danger
        onConfirm={() => {
          const c = confirmDel;
          setConfirmDel(null);
          if (c) void doDelete(c);
        }}
      />
    </>
  );
}

function CertRow({ cert }: { cert: CertReport["certs"][number] }) {
  const t = useT();
  const Icon =
    cert.status === "expired"
      ? ShieldX
      : cert.status === "critical"
        ? ShieldAlert
        : cert.status === "warn"
          ? ShieldAlert
          : cert.filePresent
            ? ShieldCheck
            : ShieldQuestion;
  const tone =
    cert.status === "expired" || cert.status === "critical" || !cert.filePresent
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
            {cert.kind === "ca" ? "CA" : "site"}
          </Badge>
          <span className={cn("shrink-0 text-[10.5px]", tone)}>
            <CalendarClock className="mr-0.5 inline h-3 w-3" />
            {cert.daysLeft < 0
              ? t("cert.expired")
              : t("cert.daysLeft").replace("{n}", String(cert.daysLeft))}
          </span>
        </div>
        {cert.advice && <p className="mt-0.5 text-[11px] text-muted">{cert.advice}</p>}
        {cert.usedBySites.length > 0 && (
          <p className="mt-0.5 text-[10.5px] text-faint">
            {t("cert.usedBy")}: {cert.usedBySites.join(", ")}
          </p>
        )}
      </div>
    </div>
  );
}
