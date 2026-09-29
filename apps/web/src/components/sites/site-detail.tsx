"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ExternalLink, FolderOpen, RefreshCw, Trash2, ScrollText, Square } from "lucide-react";
import { CustomRewriteSelect } from "./custom-rewrite-select";
import type { Site, RewritePreset } from "@nsb/schema";
import { useT, useUI } from "@/lib/store";
import { cn, cmpVersionDesc, normalizeProxyTarget, isPhpSiteSettingValid, APPLICATION_RUNTIMES, applicationRuntime, validApplication } from "@/lib/utils";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { usePackages, useService, useInvalidate, toastError, siteUrl } from "@/lib/hooks";
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
import { EnvEditor, type EnvEditorHandle, type EnvEditorState } from "./env-editor";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog } from "@/components/shared/misc";

import { SiteCertificateSelect, SiteHttpsRedirectSelect, useSiteCertificateSelection } from "./site-certificate-select";
import { SitePhpSettings } from "./site-php-settings";
import { ProjectPlatformCheck } from "./project-platform-check";
import { SiteApplicationFields } from "./site-application-fields";
import { SiteRedirectFields, DEFAULT_REDIRECT } from "./site-redirect-fields";
import { siteRedirectTarget, siteCorsProblem, siteAccessProblem, siteProxyProblem } from "@/lib/utils";
import { SiteCorsSettings } from "./site-cors-settings";
import { SiteAccessSettings } from "./site-access-settings";
import { SiteProxyRules } from "./site-proxy-rules";
import { SiteFileBackups } from "./site-file-backups";

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
  const queryClient = useQueryClient();
  const { data: packages } = usePackages();
  const { data: applicationStatus } = useService(site ? `site-app:${site.id}` : undefined);
  const [saving, setSaving] = React.useState(false);
  const savingRef = React.useRef(false);
  const [deleting, setDeleting] = React.useState(false);
  const deletingRef = React.useRef(false);
  const [deleteError, setDeleteError] = React.useState<AppErrorShape | null>(null);
  const [reloading, setReloading] = React.useState(false);
  const reloadingRef = React.useRef(false);
  const [formError, setFormError] = React.useState<AppErrorShape | null>(null);
  const saveErrorRef = React.useRef<HTMLDivElement>(null);
  const [discardOpen, setDiscardOpen] = React.useState(false);
  const [deleteOpen, setDeleteOpen] = React.useState(false);
  const [delHosts, setDelHosts] = React.useState(true);
  const [delCerts, setDelCerts] = React.useState(true);
  const [tab, setTab] = React.useState("general");
  const phpPanelRef = React.useRef<HTMLDivElement>(null);
  const phpFocusObserver = React.useRef<MutationObserver | null>(null);
  const envEditorRef = React.useRef<EnvEditorHandle>(null);
  const [envState, setEnvState] = React.useState<EnvEditorState>({ dirty: false, busy: false, canSave: false, fileName: ".env" });
  const [filesBusy, setFilesBusy] = React.useState(false);
  const filesBusyRef = React.useRef(false);
  const openingRestored = React.useRef(false);
  const onFilesBusyChange = React.useCallback((value: boolean) => { filesBusyRef.current = value; setFilesBusy(value); }, []);

  const [draft, setDraft] = React.useState<Site | null>(site);
  const [baseline, setBaseline] = React.useState<Site | null>(site);
  const [domainsInput, setDomainsInput] = React.useState(site?.domains.join(", ") ?? "");
  React.useEffect(() => {
    // 状态轮询不能清空正在编辑的表单；仅切换站点时载入草稿。
    if (site) openingRestored.current = false;
    setDraft(site);
    setBaseline(site);
    setDomainsInput(site?.domains.join(", ") ?? "");
    setFormError(null);
    setDeleteOpen(false);
    setDeleteError(null);
    setDiscardOpen(false);
    setTab("general");
    setEnvState({ dirty: false, busy: false, canSave: false, fileName: ".env" });
    return () => phpFocusObserver.current?.disconnect();
  }, [site?.id]);
  React.useEffect(() => {
    if (formError) saveErrorRef.current?.focus();
  }, [formError]);

  const certificateSelection = useSiteCertificateSelection(draft?.runtime ?? {}, domainsInput.split(/[,，\s]+/).filter(Boolean), !!site && !!draft && (draft.https || !!draft.runtime.importedCertId || !!draft.runtime.acmeCertId));

  if (!site || !draft) return null;

  const phpVersions = packages
    .filter((p) => p.id === "php" && p.install)
    .map((p) => p.version)
    .sort(cmpVersionDesc);

  const url = siteUrl(site);

  const siteDirty = !!baseline && (
    draft.name !== baseline.name || domainsInput !== baseline.domains.join(", ") ||
    draft.rootDir !== baseline.rootDir || draft.https !== baseline.https ||
    draft.rewrite !== baseline.rewrite || JSON.stringify(draft.runtime) !== JSON.stringify(baseline.runtime) ||
    JSON.stringify(draft.phpOverrides ?? {}) !== JSON.stringify(baseline.phpOverrides ?? {})
  );
  const dirty = siteDirty || envState.dirty;
  const siteBusy = saving || deleting || reloading;
  const busy = siteBusy || envState.busy || filesBusy;
  const directoryChanged = draft.rootDir.trim() !== baseline?.rootDir;
  const isRedirect = draft.runtime.kind === "redirect";
  const corsProblem = siteCorsProblem(draft.runtime.cors);
  const accessProblem = siteAccessProblem(draft.runtime.access);
  const proxyRulesProblem = siteProxyProblem(draft.runtime);
  const redirectResult = siteRedirectTarget(draft.runtime.redirect, domainsInput.split(/[,，\s]+/).filter(Boolean));
  const redirectInvalid = isRedirect && !!redirectResult.error;
  const isProxy = draft.runtime.kind !== "php" && draft.runtime.kind !== "static" && !isRedirect;
  const normalizedProxyTarget = normalizeProxyTarget(draft.runtime.proxyTarget ?? "");
  const proxyInvalid = isProxy && !normalizedProxyTarget;
  const appRuntime = applicationRuntime(draft.runtime.kind);
  const appVersions = packages.filter((p) => p.id === appRuntime?.id && p.install).map((p) => p.version).sort(cmpVersionDesc);
  const applicationInvalid = !!draft.runtime.application && (!validApplication(draft.runtime.application, draft.runtime.proxyTarget ?? "") || !appVersions.includes(draft.runtime.application.version));
  const applicationBusy = !!applicationStatus && (!!applicationStatus.pids.length || ["running", "starting", "stopping"].includes(applicationStatus.state));
  const phpInvalid = draft.runtime.kind === "php" && Object.entries(draft.phpOverrides ?? {}).some(([key, value]) => !isPhpSiteSettingValid(key, value, baseline?.phpOverrides?.[key]));
  const requestClose = () => {
    if (busy || savingRef.current || deletingRef.current || reloadingRef.current || filesBusyRef.current || envEditorRef.current?.isBusy()) return;
    if (dirty) setDiscardOpen(true);
    else onClose();
  };
  const save = async () => {
    if (busy || savingRef.current || deletingRef.current || reloadingRef.current || filesBusyRef.current || envEditorRef.current?.isBusy() || (directoryChanged && envState.dirty) || proxyInvalid || redirectInvalid || !!corsProblem || !!accessProblem || !!proxyRulesProblem || applicationInvalid || phpInvalid || (draft.https && certificateSelection.problem)) return;
    const domains = [...new Set(domainsInput.split(/[,，\s]+/).filter(Boolean).map((d) => d.toLowerCase()))];
    if (!draft.name.trim() || (!isRedirect && !draft.rootDir.trim()) || !domains.length) {
      setFormError({ code: "REQUIRED_FIELDS", message: t("detail.requiredFields") });
      return;
    }
    setFormError(null);
    savingRef.current = true;
    setSaving(true);
    try {
      const next = await api.updateSite({ ...draft, name: draft.name.trim(), rootDir: draft.rootDir.trim(), domains,
        runtime: isProxy ? { ...draft.runtime, proxyTarget: normalizedProxyTarget! } : isRedirect ? { ...draft.runtime, redirect: { ...draft.runtime.redirect!, target: redirectResult.url! } } : draft.runtime });
      queryClient.setQueryData<Site[]>(["sites"], (sites) => sites?.map((item) => item.id === next.id ? next : item));
      toast.success(t(draft.runtime.kind === "php" && (Object.keys(draft.phpOverrides ?? {}).length > 0 || Object.keys(baseline?.phpOverrides ?? {}).length > 0) ? "sites.php.saved" : "detail.updated"));
      setDraft(next);
      setBaseline(next);
      setDomainsInput(next.domains.join(", "));
    } catch (e) {
      const error = normalizeError(e);
      setFormError(error);
    } finally {
      // 恢复不完整时也刷新真实状态；站点 id 不变，已有草稿不会被轮询覆盖。
      invalidate("sites", "hosts", "certs", "services");
      savingRef.current = false;
      setSaving(false);
    }
  };

  const doDelete = async () => {
    if (busy || savingRef.current || deletingRef.current || reloadingRef.current || filesBusyRef.current || envEditorRef.current?.isBusy()) return;
    deletingRef.current = true;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteSite(site.id, { hosts: delHosts, certs: delCerts });
      toast.success(`${t("detail.deletedP1")} ${site.name} ${t("detail.deletedP2")}`);
      setDeleteOpen(false);
      onClose();
    } catch (e) {
      setDeleteError(normalizeError(e));
      // 部分恢复失败或暂存清理失败时，刷新列表可能使详情关闭，通知仍需保留。
      toastError(e, t("detail.deleteFailed"));
    } finally {
      invalidate("sites", "hosts", "certs", "services");
      deletingRef.current = false;
      setDeleting(false);
    }
  };

  return (
    <Sheet open={!!site} onOpenChange={(o) => !o && requestClose()}>
      <SheetContent onCloseAutoFocus={(event) => { if (openingRestored.current) event.preventDefault(); }} className="flex w-[calc(100vw_-_1.5rem)] flex-col gap-0 overflow-hidden sm:max-w-[640px]">
        <SheetHeader className="shrink-0 pr-14 pb-4">
          <SheetTitle className="flex min-w-0 items-center gap-2">
            <span className="truncate">{site.name}</span>
            <Badge variant={site.status === "running" ? "running" : site.status === "error" ? "error" : "muted"}>
              {site.status === "unconfigured" ? t("sites.unconfigured") : t(("state." + site.status) as "state.running")}
            </Badge>
          </SheetTitle>
          <SheetDescription className="flex items-center gap-2">
            <a href={url || undefined} onClick={(e) => { e.preventDefault(); api.openSite(site.id).catch(toastError); }} className="truncate font-mono text-primary hover:underline">
              {url || t("sites.addressPending")}
            </a>
          </SheetDescription>
        </SheetHeader>

        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-5 pb-5 sm:px-6">
          {/* 快捷操作 */}
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="secondary" size="sm" onClick={() => api.openSite(site.id).catch(toastError)}>
              <ExternalLink className="h-3.5 w-3.5" /> {t("detail.browser")}
            </Button>
            {!isRedirect && <Button variant="secondary" size="sm" onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
              <FolderOpen className="h-3.5 w-3.5" /> {t("detail.dirBtn")}
            </Button>}
            <Button variant="secondary" size="sm" disabled={busy || dirty} onClick={() => router.push("/logs?service=" + encodeURIComponent("site:" + site.id))}>
              <ScrollText className="h-3.5 w-3.5" /> {t("detail.logs")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={busy || dirty}
              onClick={async () => {
                if (busy || dirty || savingRef.current || deletingRef.current || reloadingRef.current || filesBusyRef.current || envEditorRef.current?.isBusy()) return;
                reloadingRef.current = true;
                setReloading(true);
                try {
                  await api.startSite(site.id);
                  toast.success(t("detail.reloaded"));
                } catch (e) {
                  toastError(e);
                } finally {
                  invalidate("sites", "services", "hosts");
                  reloadingRef.current = false;
                  setReloading(false);
                }
              }}
            >
              <RefreshCw className={`h-3.5 w-3.5 ${reloading ? "animate-spin" : ""}`} /> {t(site.status === "running" ? "detail.reload" : "common.start")}
            </Button>
            {site.runtime.application && <Button variant="secondary" size="sm" disabled={busy || (!applicationBusy && site.status === "stopped")}
              onClick={async () => {
                if (busy || savingRef.current || deletingRef.current || reloadingRef.current || filesBusyRef.current || envEditorRef.current?.isBusy()) return;
                reloadingRef.current = true; setReloading(true);
                try { await api.stopSite(site.id); }
                catch (error) { toastError(error); }
                finally { invalidate("sites", "services", "hosts"); reloadingRef.current = false; setReloading(false); }
              }}><Square className="size-3.5" />{t("appProcess.stop")}</Button>}
          </div>
          <p className="text-xs leading-relaxed text-faint">{t("detail.reloadHint").replace("{server}", site.runtime.webServer === "caddy" ? "Caddy" : site.runtime.webServer === "apache" ? "Apache" : "Nginx")}</p>

          <Tabs value={tab} onValueChange={setTab}>
            <TabsList className="mb-5 grid h-auto w-full grid-cols-2 gap-1 rounded-2xl sm:flex sm:w-auto sm:flex-wrap">
              <TabsTrigger value="general" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("detail.general")}</TabsTrigger>
              <TabsTrigger value="access" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("siteAccess.title")}{accessProblem && <span className="text-error" aria-label={t("siteAccess.review")}>!</span>}</TabsTrigger>
              <TabsTrigger value="cors" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("cors.title")}{corsProblem && <span className="text-error" aria-label={t("cors.review")}>!</span>}</TabsTrigger>
              {!isRedirect && <TabsTrigger value="proxy-rules" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("proxyRules.title")}{proxyRulesProblem && <span className="text-error" aria-label={t("proxyRules.review")}>!</span>}</TabsTrigger>}
              {!isRedirect && <TabsTrigger value="environment" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("env.title")}</TabsTrigger>}
              {!isRedirect && <TabsTrigger value="files" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("siteFiles.title")}</TabsTrigger>}
              {isProxy && <TabsTrigger value="application" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">{t("appProcess.title")}{applicationInvalid && <span className="text-error" aria-label={t("appProcess.invalid")}>!</span>}</TabsTrigger>}
              {draft.runtime.kind === "php" && <TabsTrigger value="php" disabled={filesBusy} className="px-2 text-[11px] sm:px-3 sm:text-[13px]">PHP{phpInvalid && <span className="text-error" aria-label={t("sites.php.review")}>!</span>}</TabsTrigger>}
            </TabsList>
            <TabsContent value="access" forceMount className="mt-0 data-[state=inactive]:hidden">
              <SiteAccessSettings key={site.id} value={draft.runtime.access} disabled={busy} onChange={(access) => setDraft({ ...draft, runtime: { ...draft.runtime, access } })} />
            </TabsContent>
            <TabsContent value="cors" forceMount className="mt-0 data-[state=inactive]:hidden">
              <SiteCorsSettings key={site.id} value={draft.runtime.cors} disabled={busy} onChange={(cors) => setDraft({ ...draft, runtime: { ...draft.runtime, cors } })} />
            </TabsContent>
            {!isRedirect && <TabsContent value="proxy-rules" forceMount className="mt-0 data-[state=inactive]:hidden">
              <SiteProxyRules siteId={site.id} value={draft.runtime.proxyRules} disabled={busy} onChange={(proxyRules) => setDraft({ ...draft, runtime: { ...draft.runtime, proxyRules } })} />
              {proxyRulesProblem && <p role="alert" className="mt-3 text-xs leading-relaxed text-error">{t(proxyRulesProblem)}</p>}
            </TabsContent>}
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
          {!isRedirect && <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-root">{t("detail.rootDir")}</Label>
            <div className="flex gap-2">
              <Input
                id="site-edit-root"
                disabled={busy || applicationBusy}
                value={draft.rootDir}
                onChange={(e) => setDraft({ ...draft, rootDir: e.target.value })}
                className="min-w-0 flex-1 font-mono text-[12.5px]"
              />
              <Button
                variant="secondary"
                disabled={busy || applicationBusy || !isTauri}
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
          </div>}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-server">{t("sites.wizard.webServer")}</Label>
            <Select value={draft.runtime.webServer} disabled={busy} onValueChange={(webServer: Site["runtime"]["webServer"]) => setDraft({ ...draft, runtime: { ...draft.runtime, webServer, customRewrite: undefined } })}>
              <SelectTrigger id="site-edit-server"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="nginx">Nginx</SelectItem>
                <SelectItem value="apache">Apache</SelectItem>
                <SelectItem value="caddy">Caddy</SelectItem>
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
                    disabled={busy}
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

          {isProxy && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="site-edit-proxy">{t("sites.detail.proxy")}</Label>
              <Input
                id="site-edit-proxy"
                disabled={busy || applicationBusy}
                value={draft.runtime.proxyTarget ?? ""}
                onChange={(e) => setDraft({ ...draft, runtime: { ...draft.runtime, proxyTarget: e.target.value } })}
                className="font-mono text-[12.5px]"
                placeholder="https://api.example.com"
                spellCheck={false}
                autoCapitalize="none"
                aria-invalid={proxyInvalid}
                aria-describedby="site-edit-proxy-hint site-edit-proxy-result"
              />
              <p id="site-edit-proxy-hint" className="text-xs leading-relaxed text-muted">{t("wz.proxyHint")}</p>
              {applicationBusy && <p className="text-xs leading-relaxed text-muted">{t("appProcess.stopBeforeEdit")}</p>}
              <p id="site-edit-proxy-result" aria-live="polite" className={cn("text-xs leading-relaxed [overflow-wrap:anywhere]", normalizedProxyTarget ? "text-muted" : "text-error")}>
                {normalizedProxyTarget ? t("sites.proxy.effectiveTarget").replace("{target}", normalizedProxyTarget) : t("sites.proxy.invalidTarget")}
              </p>
            </div>
          )}

          {isRedirect && <SiteRedirectFields id="detail-redirect" value={draft.runtime.redirect ?? DEFAULT_REDIRECT} domains={domainsInput.split(/[,，\s]+/).filter(Boolean)} disabled={busy}
            onChange={(redirect) => setDraft({ ...draft, runtime: { ...draft.runtime, redirect } })} />}

          {/* HTTPS */}
          <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
            <div className="flex flex-col gap-0.5">
              <span className="text-[12.5px] font-medium">HTTPS</span>
              <span className="text-[11px] text-faint">{t("sites.detail.httpsSelectHint")}</span>
            </div>
            <Switch
              aria-label="HTTPS"
              checked={draft.https}
              onCheckedChange={(https) => setDraft({ ...draft, https, runtime: { ...draft.runtime, httpsRedirect: https ? draft.runtime.httpsRedirect : undefined } })}
              disabled={busy}
            />
          </div>

          {(draft.https || draft.runtime.importedCertId || draft.runtime.acmeCertId) && (
            <SiteCertificateSelect id="site-edit-cert" selection={certificateSelection} disabled={busy}
              onChange={(binding) => setDraft({ ...draft, runtime: { ...draft.runtime, ...binding } })} />
          )}
          {draft.https && <SiteHttpsRedirectSelect id="site-edit-https-redirect" value={draft.runtime.httpsRedirect} disabled={busy}
            onChange={(httpsRedirect) => setDraft({ ...draft, runtime: { ...draft.runtime, httpsRedirect } })} />}

          {/* 伪静态 */}
          {!isRedirect && <div className="flex flex-col gap-1.5">
            <Label htmlFor="site-edit-rewrite">{t("sites.detail.rewrite")}</Label>
            <Select
              value={draft.rewrite}
              disabled={busy}
              onValueChange={(v) => setDraft({ ...draft, rewrite: v as RewritePreset, runtime: { ...draft.runtime, customRewrite: undefined } })}
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
            {(draft.runtime.kind === "php" || draft.runtime.kind === "static") && <CustomRewriteSelect server={draft.runtime.webServer} value={draft.runtime.customRewrite} disabled={busy} onChange={(customRewrite) => setDraft({ ...draft, runtime: { ...draft.runtime, customRewrite } })} />}
          </div>}

            </TabsContent>
            {isProxy && <TabsContent value="application" forceMount className="mt-0 space-y-4 data-[state=inactive]:hidden">
              <div className="space-y-1.5">
                <Label htmlFor="site-edit-app-kind">{t("appProcess.runtime")}</Label>
                <Select value={draft.runtime.kind} disabled={busy || applicationBusy}
                  onValueChange={(kind: Site["runtime"]["kind"]) => setDraft({ ...draft, runtime: { ...draft.runtime, kind, customRewrite: undefined, application: undefined } })}>
                  <SelectTrigger id="site-edit-app-kind"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="reverse-proxy">{t("wz.kindProxy")}</SelectItem>
                    {APPLICATION_RUNTIMES.map((runtime) => <SelectItem key={runtime.kind} value={runtime.kind}>{runtime.label}</SelectItem>)}
                  </SelectContent>
                </Select>
              </div>
              {applicationStatus && <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-fill p-3 text-xs">
                <span>{t("appProcess.state")} · {t(("state." + applicationStatus.state) as "state.running")}</span>
                <Button variant="ghost" size="sm" disabled={busy || dirty} onClick={() => router.push(`/logs?service=${encodeURIComponent(`site-app:${site.id}`)}`)}><ScrollText className="size-3.5" />{t("appProcess.logs")}</Button>
              </div>}
              {appRuntime ? <SiteApplicationFields id="site-edit-app" kind={draft.runtime.kind} value={draft.runtime.application} versions={appVersions}
                rootDir={draft.rootDir} disabled={busy} locked={applicationBusy} onChange={(application) => setDraft({ ...draft, runtime: { ...draft.runtime, application } })} />
                : <p className="text-xs leading-relaxed text-muted">{t("appProcess.externalHint")}</p>}
              {applicationInvalid && <p role="alert" className="text-xs leading-relaxed text-error">{t("appProcess.invalid")}</p>}
            </TabsContent>}
            {!isRedirect && <TabsContent value="environment" forceMount className="mt-0 data-[state=inactive]:hidden">
              <EnvEditor key={`${site.id}:${baseline?.rootDir}`} ref={envEditorRef} siteId={site.id} disabled={siteBusy || filesBusy}
                directoryChanged={directoryChanged} onStateChange={setEnvState} />
            </TabsContent>}
            {!isRedirect && <TabsContent value="files" forceMount className="mt-0 data-[state=inactive]:hidden">
              <SiteFileBackups key={site.id} siteId={site.id} revision={baseline?.updatedAt ?? site.updatedAt} active={tab === "files"}
                disabled={siteBusy || envState.busy} dirty={dirty} onBusyChange={onFilesBusyChange}
                onCreateFromRestored={(project) => {
                  if (dirty || busy || filesBusyRef.current || savingRef.current || deletingRef.current || reloadingRef.current || envEditorRef.current?.isBusy()) return;
                  openingRestored.current = true;
                  onClose();
                  useUI.getState().openExistingProject(project);
                }} />
            </TabsContent>}
            {draft.runtime.kind === "php" && <TabsContent ref={phpPanelRef} value="php" forceMount className="mt-0 space-y-5 data-[state=inactive]:hidden">
              <ProjectPlatformCheck siteId={site.id} savedRoot={baseline?.rootDir} version={draft.runtime.phpVersion ?? ""} disabled={busy} directoryChanged={directoryChanged} />
              <SitePhpSettings values={draft.phpOverrides ?? {}} previousValues={baseline?.phpOverrides ?? {}} rootDir={draft.rootDir} disabled={busy}
                onChange={(phpOverrides) => setDraft({ ...draft, phpOverrides })} />
            </TabsContent>}
          </Tabs>

          {/* 删除 */}
          <div className="border-t border-dashed border-separator pt-4">
            <Button variant="ghost" disabled={busy} onClick={() => { setDeleteError(null); setDeleteOpen(true); }} className="text-destructive">
              <Trash2 className="h-3.5 w-3.5" /> {t("common.delete")}
            </Button>
            <ConfirmDialog
              open={deleteOpen}
              onOpenChange={(open) => { if (!deletingRef.current) setDeleteOpen(open); }}
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
                  <Switch checked={delHosts} disabled={deleting} onCheckedChange={setDelHosts} />
                </label>
                <label className="flex cursor-pointer items-center justify-between gap-3">
                  <span>{t("sites.detail.cert")}</span>
                  <Switch checked={delCerts} disabled={deleting} onCheckedChange={setDelCerts} />
                </label>
                <p className="text-[11px] leading-relaxed text-muted">{t("sites.detail.deleteCertHint")}</p>
              </div>
              {deleteError && <div role="alert" className="space-y-2 rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">
                <p>{deleteError.message}</p>
                {deleteError.hint && <p>{deleteError.hint}</p>}
                {deleteError.detail && <details><summary className="cursor-pointer">{t("sites.detail.deleteErrorDetail")}</summary><p className="mt-2 whitespace-pre-wrap font-mono">{deleteError.detail}</p></details>}
              </div>}
            </ConfirmDialog>

          </div>
        </div>
        <div className="mx-5 shrink-0 border-t border-dashed border-separator py-4 sm:mx-6">
          {tab !== "environment" && envState.dirty && <button className="mb-2 text-left text-xs text-warn underline" onClick={() => setTab("environment")}>{t("env.pendingElsewhere")}</button>}
          {accessProblem && <button className="mb-2 mr-3 text-left text-xs text-error underline" onClick={() => { setTab("access"); requestAnimationFrame(() => document.querySelector<HTMLElement>('[id^="site-access-"][aria-invalid="true"]:not(:disabled)')?.focus()); }}>{t("siteAccess.review")}</button>}
          {corsProblem && <button className="mb-2 mr-3 text-left text-xs text-error underline" onClick={() => { setTab("cors"); requestAnimationFrame(() => document.querySelector<HTMLElement>('[id^="site-cors-"][aria-invalid="true"]:not(:disabled)')?.focus()); }}>{t("cors.review")}</button>}
          {proxyRulesProblem && <button className="mb-2 mr-3 text-left text-xs text-error underline" onClick={() => { setTab("proxy-rules"); requestAnimationFrame(() => document.querySelector<HTMLElement>('[id^="site-proxy-"][aria-invalid="true"]:not(:disabled)')?.focus()); }}>{t("proxyRules.review")}</button>}
          {phpInvalid && <button className="mb-2 text-left text-xs text-error underline" onClick={() => {
            phpFocusObserver.current?.disconnect();
            const panel = phpPanelRef.current;
            const focusInvalid = () => {
              if (panel?.dataset.state !== "active") return;
              const input = panel.querySelector<HTMLElement>('[aria-invalid="true"]:not(:disabled), [data-php-error="true"]');
              input?.scrollIntoView({ block: "nearest" }); input?.focus();
              phpFocusObserver.current?.disconnect();
            };
            if (panel) {
              phpFocusObserver.current = new MutationObserver(focusInvalid);
              phpFocusObserver.current.observe(panel, { attributes: true, attributeFilter: ["data-state"] });
            }
            setTab("php"); focusInvalid();
          }}>{t("sites.php.review")}</button>}
          {formError && <div ref={saveErrorRef} tabIndex={-1} role="alert" className="mb-3 max-h-40 space-y-2 overflow-y-auto rounded-lg bg-error-soft p-3 text-xs text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]">
            <p>{formError.message}</p>
            {formError.hint && <p>{formError.hint}</p>}
            {formError.detail && <details><summary className="cursor-pointer">{t("sites.detail.deleteErrorDetail")}</summary><p className="mt-2 whitespace-pre-wrap font-mono">{formError.detail}</p></details>}
          </div>}
          <div className="flex flex-wrap items-center justify-between gap-3">
            <span className="text-xs text-muted">{(tab === "environment" ? envState.dirty : siteDirty) ? t("detail.unsaved") : t(tab === "environment" ? "env.clean" : "detail.saved")}</span>
            <div className="flex min-w-0 max-w-full gap-2">
              <Button variant="ghost" onClick={requestClose} disabled={busy}>{t(tab === "files" ? "common.close" : "common.cancel")}</Button>
              {tab === "environment" ? <Button className="min-w-0" title={t("env.saveNamed").replace("{file}", envState.fileName)} onClick={() => void envEditorRef.current?.save()} disabled={siteBusy || filesBusy || !envState.canSave}><span className="truncate">{envState.busy ? t("detail.saveBusy") : t("env.saveNamed").replace("{file}", envState.fileName)}</span></Button>
                : tab !== "files" && <Button onClick={save} disabled={busy || !siteDirty || (directoryChanged && envState.dirty) || proxyInvalid || redirectInvalid || !!corsProblem || !!accessProblem || !!proxyRulesProblem || applicationInvalid || phpInvalid || (draft.https && !!certificateSelection.problem)}>{saving ? t("detail.saveBusy") : t("common.save")}</Button>}
            </div>
          </div>
        </div>
        <ConfirmDialog open={discardOpen} onOpenChange={setDiscardOpen} title={t("detail.discardTitle")}
          description={t("detail.discardHint")} confirmText={t("detail.discard")} onConfirm={onClose} />
      </SheetContent>
    </Sheet>
  );
}
