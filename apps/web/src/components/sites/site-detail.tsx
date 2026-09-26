"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
import { ExternalLink, FolderOpen, RefreshCw, Trash2, ScrollText } from "lucide-react";
import type { Site, RewritePreset } from "@nsb/schema";
import { useT } from "@/lib/store";
import { cmpVersionDesc } from "@/lib/utils";
import { isTauri } from "@/lib/backend";
import { usePackages, useInvalidate, toastError, siteUrl, usePorts } from "@/lib/hooks";
import * as api from "@/lib/api";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
} from "@/components/ui/sheet";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { EnvEditor } from "./env-editor";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog } from "@/components/shared/misc";

const REWRITE_OPTIONS: { value: RewritePreset; label?: string; labelKey?: string }[] = [
  { value: "none", labelKey: "detail.none" },
  { value: "laravel", label: "Laravel" },
  { value: "symfony", label: "Symfony" },
  { value: "thinkphp", label: "ThinkPHP" },
  { value: "wordpress", label: "WordPress" },
  { value: "yii2", label: "Yii2" },
  { value: "codeigniter", label: "CodeIgniter 4" },
  { value: "cakephp", label: "CakePHP" },
  { value: "drupal", label: "Drupal" },
  { value: "joomla", label: "Joomla" },
  { value: "spa-fallback", label: "SPA fallback" },
  { value: "next-export", label: "Next.js (export)" },
];

export function SiteDetailSheet({
  site,
  onClose,
}: {
  site: Site | null;
  onClose: () => void;
}) {
  const t = useT();
  const router = useRouter();
  const invalidate = useInvalidate();
  const ports = usePorts();
  const importedCerts = useQuery({ queryKey: ["cert-imported"], queryFn: api.certImportedList });
  const { data: packages } = usePackages();
  const [saving, setSaving] = React.useState(false);
  const [deleting, setDeleting] = React.useState(false);
  const [reloading, setReloading] = React.useState(false);
  const [formError, setFormError] = React.useState("");
  const [discardOpen, setDiscardOpen] = React.useState(false);
  const [deleteOpen, setDeleteOpen] = React.useState(false);
  const [delHosts, setDelHosts] = React.useState(true);
  const [delCerts, setDelCerts] = React.useState(true);

  const [draft, setDraft] = React.useState<Site | null>(site);
  const [baseline, setBaseline] = React.useState<Site | null>(site);
  const [domainsInput, setDomainsInput] = React.useState(site?.domains.join(", ") ?? "");
  React.useEffect(() => {
    // 状态轮询不能清空正在编辑的表单；仅切换站点时载入草稿。
    setDraft(site);
    setBaseline(site);
    setDomainsInput(site?.domains.join(", ") ?? "");
    setFormError("");
    setDeleteOpen(false);
    setDiscardOpen(false);
  }, [site?.id]);

  if (!site || !draft) return null;

  const phpVersions = packages
    .filter((p) => p.id === "php" && p.install)
    .map((p) => p.version)
    .sort(cmpVersionDesc);

  const url = siteUrl(site, ports);

  const dirty = !!baseline && (
    draft.name !== baseline.name || domainsInput !== baseline.domains.join(", ") ||
    draft.rootDir !== baseline.rootDir || draft.https !== baseline.https ||
    draft.rewrite !== baseline.rewrite || JSON.stringify(draft.runtime) !== JSON.stringify(baseline.runtime)
  );
  const busy = saving || deleting || reloading;
  const requestClose = () => {
    if (busy) return;
    if (dirty) setDiscardOpen(true);
    else onClose();
  };
  const save = async () => {
    if (busy) return;
    const domains = [...new Set(domainsInput.split(/[,，\s]+/).filter(Boolean).map((d) => d.toLowerCase()))];
    if (!draft.name.trim() || !draft.rootDir.trim() || !domains.length) {
      setFormError(t("detail.requiredFields"));
      return;
    }
    setFormError("");
    setSaving(true);
    try {
      const next = await api.updateSite({ ...draft, name: draft.name.trim(), rootDir: draft.rootDir.trim(), domains });
      toast.success(t("detail.updated"));
      invalidate("sites", "hosts", "certs", "services");
      setDraft(next);
      setBaseline(next);
      setDomainsInput(next.domains.join(", "));
    } catch (e) {
      toastError(e, t("detail.updateFailed"));
    } finally {
      setSaving(false);
    }
  };

  const doDelete = async () => {
    if (deleting) return;
    setDeleting(true);
    try {
      await api.deleteSite(site.id, { hosts: delHosts, certs: delCerts });
      toast.success(`${t("detail.deletedP1")} ${site.name} ${t("detail.deletedP2")}`);
      setDeleteOpen(false);
      onClose();
      invalidate("sites", "hosts", "certs");
    } catch (e) {
      toastError(e, t("detail.deleteFailed"));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <Sheet open={!!site} onOpenChange={(o) => !o && requestClose()}>
      <SheetContent className="flex w-[calc(100vw-1.5rem)] flex-col gap-0 overflow-hidden sm:max-w-[640px]">
        <SheetHeader className="shrink-0 pr-14 pb-4">
          <SheetTitle className="flex min-w-0 items-center gap-2">
            <span className="truncate">{site.name}</span>
            <Badge variant={site.status === "running" ? "running" : site.status === "error" ? "error" : "muted"}>
              {site.status === "unconfigured" ? t("sites.unconfigured") : t(("state." + site.status) as "state.running")}
            </Badge>
          </SheetTitle>
          <SheetDescription className="flex items-center gap-2">
            <a href={url} onClick={(e) => { e.preventDefault(); api.openInBrowser(url).catch(toastError); }} className="truncate font-mono text-primary hover:underline">
              {url}
            </a>
          </SheetDescription>
        </SheetHeader>

        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-5 pb-5 sm:px-6">
          {/* 快捷操作 */}
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="secondary" size="sm" onClick={() => api.openInBrowser(url).catch(toastError)}>
              <ExternalLink className="h-3.5 w-3.5" /> {t("detail.browser")}
            </Button>
            <Button variant="secondary" size="sm" onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
              <FolderOpen className="h-3.5 w-3.5" /> {t("detail.dirBtn")}
            </Button>
            <Button variant="secondary" size="sm" onClick={() => router.push("/logs?service=" + encodeURIComponent(site.runtime.webServer === "apache" ? "apache" : "site:" + site.id))}>
              <ScrollText className="h-3.5 w-3.5" /> {t("detail.logs")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={busy || dirty}
              onClick={async () => {
                setReloading(true);
                try {
                  await api.restartService(site.runtime.webServer);
                  toast.success(t("detail.reloaded"));
                  invalidate("sites", "services");
                } catch (e) {
                  toastError(e);
                } finally {
                  setReloading(false);
                }
              }}
            >
              <RefreshCw className="h-3.5 w-3.5" /> {t("detail.reload")}
            </Button>
          </div>

          <Tabs defaultValue="general">
            <TabsList className="mb-5">
              <TabsTrigger value="general">{t("detail.general")}</TabsTrigger>
              <TabsTrigger value="environment">{t("env.title")}</TabsTrigger>
            </TabsList>
            <TabsContent value="general" forceMount className="mt-0 space-y-5 data-[state=inactive]:hidden">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-name">{t("sites.wizard.name")}</Label>
            <Input id="site-edit-name" value={draft.name} disabled={busy} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
          </div>
          {/* 域名 */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-domains">{t("detail.domainsLabel")}</Label>
            <Input
              id="site-edit-domains"
              disabled={busy}
              value={domainsInput}
              onChange={(e) => setDomainsInput(e.target.value)}
              className="font-mono text-[12.5px]"
            />
          </div>

          {/* 根目录 */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-root">{t("detail.rootDir")}</Label>
            <div className="flex gap-2">
              <Input
                id="site-edit-root"
                disabled={busy}
                value={draft.rootDir}
                onChange={(e) => setDraft({ ...draft, rootDir: e.target.value })}
                className="min-w-0 flex-1 font-mono text-[12.5px]"
              />
              <Button
                variant="secondary"
                disabled={busy || !isTauri}
                title={!isTauri ? t("detail.desktopFolder") : undefined}
                onClick={async () => {
                  try {
                    const { open } = await import("@tauri-apps/plugin-dialog");
                    const picked = await open({ directory: true });
                    if (typeof picked === "string") setDraft({ ...draft, rootDir: picked });
                  } catch (e) {
                    toastError(e);
                  }
                }}
              >
                  {t("detail.select")}
                  </Button>
            </div>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-server">{t("sites.wizard.webServer")}</Label>
            <Select value={draft.runtime.webServer} disabled={busy} onValueChange={(webServer: Site["runtime"]["webServer"]) => setDraft({ ...draft, runtime: { ...draft.runtime, webServer } })}>
              <SelectTrigger id="site-edit-server"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="nginx">Nginx</SelectItem>
                <SelectItem value="apache">Apache</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {/* 运行时 */}
          {draft.runtime.kind === "php" && (
            <div className="flex flex-col gap-1.5">
              <Label>{t("sites.detail.php")}</Label>
              <div className="flex flex-wrap gap-2">
                {(phpVersions.length ? phpVersions : [draft.runtime.phpVersion ?? ""]).filter(Boolean).map((v) => (
                  <button
                    key={v}
                    disabled={saving}
                    onClick={() => setDraft({ ...draft, runtime: { ...draft.runtime, phpVersion: v } })}
                    className={`rounded-lg border px-3 py-1.5 font-mono text-[12px] transition-all ${
                      draft.runtime.phpVersion === v
                        ? "border-primary/60 bg-primary-soft text-primary"
                        : "border-border text-muted hover:border-border-strong"
                    }`}
                  >
                    PHP {v}
                  </button>
                ))}
                {phpVersions.length === 0 && (
                  <p className="text-[11.5px] text-faint">{t("detail.noOtherPhp")}</p>
                )}
              </div>
            </div>
          )}

          {draft.runtime.kind !== "php" && draft.runtime.kind !== "static" && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="site-edit-proxy">{t("sites.detail.proxy")}</Label>
              <Input
                id="site-edit-proxy"
                disabled={busy}
                value={draft.runtime.proxyTarget ?? ""}
                onChange={(e) => setDraft({ ...draft, runtime: { ...draft.runtime, proxyTarget: e.target.value } })}
                className="font-mono text-[12.5px]"
                placeholder="https://api.example.com"
              />
            </div>
          )}

          {/* HTTPS */}
          <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
            <div className="flex flex-col gap-0.5">
              <span className="text-[12.5px] font-medium">HTTPS</span>
              <span className="text-[11px] text-faint">{t("sites.wizard.httpsHint")}</span>
            </div>
            <Switch
              aria-label="HTTPS"
              checked={draft.https}
              onCheckedChange={(https) => setDraft({ ...draft, https })}
              disabled={saving}
            />
          </div>

          {/* 伪静态 */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-rewrite">{t("sites.detail.rewrite")}</Label>
            <Select
              value={draft.rewrite}
              disabled={busy}
              onValueChange={(v) => setDraft({ ...draft, rewrite: v as RewritePreset })}
            >
              <SelectTrigger id="site-edit-rewrite">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {REWRITE_OPTIONS.map((r) => (
                  <SelectItem key={r.value} value={r.value}>
                    {r.labelKey ? t(r.labelKey as never) : r.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

            </TabsContent>
            <TabsContent value="environment" forceMount className="mt-0 data-[state=inactive]:hidden">
              <EnvEditor siteId={site.id} />
            </TabsContent>
          </Tabs>

          {/* 删除 */}
          <div className="border-t border-dashed border-separator pt-4">
            <Button variant="ghost" disabled={busy} onClick={() => setDeleteOpen(true)} className="text-destructive">
              <Trash2 className="h-3.5 w-3.5" /> {t("common.delete")}
            </Button>
            <ConfirmDialog
              open={deleteOpen}
              onOpenChange={setDeleteOpen}
              title={`${t("detail.deleteTitleP1")} ${site.name}`}
              description={t("sites.deleteConfirm")}
              confirmText={t("detail.confirmDelete")}
              danger
              loading={deleting}
              onConfirm={doDelete}
            >
              <div className="flex flex-col gap-3 rounded-xl bg-fill p-3.5 text-[12.5px]">
                <label className="flex cursor-pointer items-center justify-between gap-3">
                  <span>{t("sites.detail.hostsRecord")}</span>
                  <Switch checked={delHosts} onCheckedChange={setDelHosts} />
                </label>
                <label className="flex cursor-pointer items-center justify-between gap-3">
                  <span>{t("sites.detail.cert")}</span>
                  <Switch checked={delCerts} onCheckedChange={setDelCerts} />
                </label>
              </div>
            </ConfirmDialog>

          </div>
        </div>
        <div className="mx-5 shrink-0 border-t border-dashed border-separator py-4 sm:mx-6">
          {formError && <p role="alert" className="mb-3 text-sm text-error">{formError}</p>}
          <div className="flex items-center justify-between gap-3">
            <span className="text-xs text-muted">{dirty ? t("detail.unsaved") : t("detail.saved")}</span>
            <div className="flex gap-2">
              <Button variant="ghost" onClick={requestClose} disabled={busy}>{t("common.cancel")}</Button>
              <Button onClick={save} disabled={busy || !dirty}>{saving ? t("detail.saveBusy") : t("common.save")}</Button>
            </div>
          </div>

          {(draft.https || draft.runtime.importedCertId) && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="site-edit-cert">{t("sites.detail.certSource")}</Label>
              <Select
                value={draft.runtime.importedCertId ?? "local"}
                disabled={busy || importedCerts.isLoading}
                onValueChange={(value) => setDraft({ ...draft, runtime: { ...draft.runtime, importedCertId: value === "local" ? undefined : value } })}
              >
                <SelectTrigger id="site-edit-cert"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="local">{t("sites.detail.localCert")}</SelectItem>
                  {importedCerts.data?.map((cert) => (
                    <SelectItem key={cert.id} value={cert.id} disabled={!cert.usable}>
                      {cert.subject}{cert.usable ? ` · ${cert.daysLeft}d` : ` · ${t("sites.detail.certInvalid")}`}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {importedCerts.error && <p className="text-[11px] text-error">{t("tls.readFailed")}</p>}
              {draft.runtime.importedCertId && !importedCerts.data?.some((cert) => cert.id === draft.runtime.importedCertId) && (
                <p className="text-[11px] text-error">{t("sites.detail.certMissing")}</p>
              )}
              <p className="text-[11px] text-faint">{t("sites.detail.certSourceHint")}</p>
            </div>
          )}
        </div>
        <ConfirmDialog open={discardOpen} onOpenChange={setDiscardOpen} title={t("detail.discardTitle")}
          description={t("detail.discardHint")} confirmText={t("detail.discard")} onConfirm={onClose} />
      </SheetContent>
    </Sheet>
  );
}
