"use client";

import * as React from "react";
import { toast } from "sonner";
import { ExternalLink, FolderOpen, RefreshCw, Trash2, ScrollText } from "lucide-react";
import type { Site, RewritePreset } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
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
import { Separator } from "@/components/ui/misc";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog } from "@/components/shared/misc";

const REWRITE_OPTIONS: { value: RewritePreset; label?: string; labelKey?: string }[] = [
  { value: "none", labelKey: "detail.none" },
  { value: "laravel", label: "Laravel / Symfony" },
  { value: "thinkphp", label: "ThinkPHP" },
  { value: "wordpress", label: "WordPress" },
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
  const invalidate = useInvalidate();
  const ports = usePorts();
  const { data: packages } = usePackages();
  const [saving, setSaving] = React.useState(false);
  const [deleteOpen, setDeleteOpen] = React.useState(false);
  const [delHosts, setDelHosts] = React.useState(true);
  const [delCerts, setDelCerts] = React.useState(true);

  const [draft, setDraft] = React.useState<Site | null>(site);
  React.useEffect(() => setDraft(site), [site]);

  if (!site || !draft) return null;

  const phpVersions = packages
    .filter((p) => p.id === "php" && p.install)
    .map((p) => p.version)
    .sort()
    .reverse();

  const url = siteUrl(site, ports.http, ports.https);

  const save = async (patch: Partial<Site>) => {
    setSaving(true);
    try {
      const next = await api.updateSite({ ...draft, ...patch, updatedAt: Date.now() });
      toast.success(t("detail.updated"));
      invalidate("sites");
      setDraft(next);
    } catch (e) {
      toastError(e, t("detail.updateFailed"));
    } finally {
      setSaving(false);
    }
  };

  const doDelete = async () => {
    try {
      await api.deleteSite(site.id, { hosts: delHosts, certs: delCerts });
      toast.success(`${t("detail.deletedP1")} ${site.name} ${t("detail.deletedP2")}`);
      setDeleteOpen(false);
      onClose();
      invalidate("sites", "hosts", "certs");
    } catch (e) {
      toastError(e, t("detail.deleteFailed"));
    }
  };

  return (
    <Sheet open={!!site} onOpenChange={(o) => !o && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-[480px]">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            {site.name}
            <Badge variant={site.status === "running" ? "running" : site.status === "error" ? "error" : "muted"}>
              {site.status}
            </Badge>
          </SheetTitle>
          <SheetDescription className="flex items-center gap-2">
            <a href={url} onClick={(e) => { e.preventDefault(); api.openInBrowser(url).catch(toastError); }} className="font-mono text-primary hover:underline">
              {url}
            </a>
          </SheetDescription>
        </SheetHeader>

        <div className="flex flex-col gap-5 p-6 pt-2">
          {/* 快捷操作 */}
          <div className="flex items-center gap-2">
            <Button variant="secondary" size="sm" onClick={() => api.openInBrowser(url).catch(toastError)}>
              <ExternalLink className="h-3.5 w-3.5" /> {t("detail.browser")}
            </Button>
            <Button variant="secondary" size="sm" onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
              <FolderOpen className="h-3.5 w-3.5" /> {t("detail.dirBtn")}
            </Button>
            <Button variant="secondary" size="sm" onClick={() => window.location.assign("/logs")}>
              <ScrollText className="h-3.5 w-3.5" /> {t("detail.logs")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={async () => {
                try {
                  await api.restartService("nginx");
                  toast.success(t("detail.reloaded"));
                  invalidate("sites");
                } catch (e) {
                  toastError(e);
                }
              }}
            >
              <RefreshCw className="h-3.5 w-3.5" /> {t("detail.reload")}
            </Button>
          </div>

          {/* 域名 */}
          <div className="flex flex-col gap-1.5">
            <Label>{t("detail.domainsLabel")}</Label>
            <Input
              value={draft.domains.join(", ")}
              onChange={(e) => setDraft({ ...draft, domains: e.target.value.split(/[,，\s]+/).filter(Boolean) })}
              className="font-mono text-[12.5px]"
            />
          </div>

          {/* 根目录 */}
          <div className="flex flex-col gap-1.5">
            <Label>{t("detail.rootDir")}</Label>
            <div className="flex gap-2">
              <Input
                value={draft.rootDir}
                onChange={(e) => setDraft({ ...draft, rootDir: e.target.value })}
                className="flex-1 font-mono text-[12.5px]"
              />
              <Button
                variant="secondary"
                onClick={async () => {
                  const { isTauri } = await import("@/lib/backend");
                  if (isTauri) {
                    const { open } = await import("@tauri-apps/plugin-dialog");
                    const picked = await open({ directory: true });
                    if (typeof picked === "string") setDraft({ ...draft, rootDir: picked });
                  }
                }}
              >
                  {t("detail.select")}
                  </Button>
            </div>
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
                    onClick={() => save({ runtime: { ...draft.runtime, phpVersion: v } })}
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

          {draft.runtime.kind === "reverse-proxy" && (
            <div className="flex flex-col gap-1.5">
              <Label>{t("sites.detail.proxy")}</Label>
              <Input
                value={draft.runtime.proxyTarget ?? ""}
                onChange={(e) => setDraft({ ...draft, runtime: { ...draft.runtime, proxyTarget: e.target.value } })}
                className="font-mono text-[12.5px]"
                placeholder="127.0.0.1:8080"
              />
            </div>
          )}

          {/* HTTPS */}
          <div className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card-2/40 p-3.5">
            <div className="flex flex-col gap-0.5">
              <span className="text-[12.5px] font-medium">HTTPS</span>
              <span className="text-[11px] text-faint">{t("sites.wizard.httpsHint")}</span>
            </div>
            <Switch
              checked={draft.https}
              onCheckedChange={(v) => save({ https: v })}
              disabled={saving}
            />
          </div>

          {/* 伪静态 */}
          <div className="flex flex-col gap-1.5">
            <Label>{t("sites.detail.rewrite")}</Label>
            <Select
              value={draft.rewrite}
              onValueChange={(v) => save({ rewrite: v as RewritePreset })}
            >
              <SelectTrigger>
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

          {/* 保存（域名/目录/代理） */}
          <Button onClick={() => save({})} disabled={saving}>
            {saving ? t("detail.saveBusy") : t("detail.saveReload")}
          </Button>

          <Separator />

          {/* 删除 */}
          <div className="flex flex-col gap-2">
            <Button variant="destructive" onClick={() => setDeleteOpen(true)} className="w-full">
              <Trash2 className="h-3.5 w-3.5" /> {t("common.delete")}
            </Button>
            <ConfirmDialog
              open={deleteOpen}
              onOpenChange={setDeleteOpen}
              title={`${t("detail.deleteTitleP1")} ${site.name}`}
              description={t("sites.deleteConfirm")}
              confirmText={t("detail.confirmDelete")}
              danger
              onConfirm={doDelete}
            >
              <div className="flex flex-col gap-3 rounded-xl border border-border bg-card-2/40 p-3.5 text-[12.5px]">
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

            {/* .env 编辑：Laravel/WordPress 类项目最常改的文件 */}
            <div>
              <div className="mb-2 flex items-center gap-2">
                <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                  {t("env.title")}
                </span>
                <span className="h-px flex-1 bg-border/60" />
              </div>
              <EnvEditor siteId={site.id} />
            </div>
          </div>
        </div>
      </SheetContent>
    </Sheet>
  );
}
