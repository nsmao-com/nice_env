"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import { Globe, Plus, ExternalLink, FolderOpen, ScrollText, Power, Settings2, TerminalSquare, FolderSearch } from "lucide-react";
import type { Site } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useSites, useInvalidate, toastError, siteUrl, usePorts } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { StatusLight } from "@/components/shared/status-light";
import { CopyButton, EmptyState } from "@/components/shared/misc";
import { PageHeader } from "@/components/layout/app-shell";
import { SiteDetailSheet } from "@/components/sites/site-detail";
import { ProjectScannerDialog } from "@/components/sites/project-scanner";

export default function SitesPage() {
  const t = useT();
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const { data: sites } = useSites();
  const [detail, setDetail] = React.useState<Site | null>(null);
  const [scanOpen, setScanOpen] = React.useState(false);
  // 命令面板/其它入口可能请求直接打开扫描对话框
  const pendingScan = useUI((st) => st.pendingScan);
  const consumeScan = useUI((st) => st.consumeScan);
  React.useEffect(() => {
    if (pendingScan) {
      setScanOpen(true);
      consumeScan();
    }
  }, [pendingScan, consumeScan]);

  return (
    <div className="pb-8">
      <PageHeader
        title={t("sites.title")}
        subtitle={t("sites.subtitle")}
        actions={
          <>
            {/* 已有项目的人多半不想手填表单，先给「扫一下」这条路 */}
            <Button variant="secondary" onClick={() => setScanOpen(true)}>
              <FolderSearch className="h-3.5 w-3.5" /> {t("scanner.scan")}
            </Button>
            <Button onClick={() => setWizardOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("sites.create")}
            </Button>
          </>
        }
      />

      {sites.length === 0 ? (
        <EmptyState
          icon={Globe}
          title={t("sites.empty")}
          hint={t("sites.emptyHint")}
          action={
            <Button size="lg" onClick={() => setWizardOpen(true)}>
              <Plus className="h-4 w-4" /> {t("sites.create")}
            </Button>
          }
          className="min-h-[420px]"
        />
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <AnimatePresence>
            {[...sites]
              .sort((a, b) => b.updatedAt - a.updatedAt)
              .map((site) => (
                <SiteCard key={site.id} site={site} onOpenDetail={() => setDetail(site)} />
              ))}
          </AnimatePresence>
        </div>
      )}

      <ProjectScannerDialog open={scanOpen} onOpenChange={setScanOpen} />

      <SiteDetailSheet site={detail} onClose={() => setDetail(null)} />
    </div>
  );
}

function SiteCard({ site, onOpenDetail }: { site: Site; onOpenDetail: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const ports = usePorts();
  const url = siteUrl(site, ports.http, ports.https);
  const running = site.status === "running";

  const toggle = async () => {
    try {
      if (running) await api.stopSite(site.id);
      else await api.startSite(site.id);
      invalidate("sites");
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <motion.div layout initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, scale: 0.96 }}>
      <Card className="group flex flex-col gap-3 p-4 transition-all hover:border-border-strong">
        <div className="flex items-start justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
              <Globe className={`h-4 w-4 ${running ? "text-running" : "text-faint"}`} strokeWidth={1.8} />
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <span className="truncate text-[13.5px] font-medium">{site.name}</span>
                <StatusLight state={site.status} size={6} />
              </div>
              <span className="truncate text-[11px] text-faint">{site.domains.join(", ")}</span>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
            <CopyButton text={url} />
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openInBrowser(url).catch(toastError)}>
                  <ExternalLink className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openBrowser")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
                  <FolderOpen className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openFolder")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openTerminal(site.rootDir).catch(toastError)}>
                  <TerminalSquare className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openTerminal")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={onOpenDetail}>
                  <Settings2 className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("sites.siteSettings")}</TooltipContent>
            </Tooltip>
          </div>
        </div>

        <div className="flex items-center gap-1.5">
          {site.https && <Badge variant="info">HTTPS</Badge>}
          <Badge variant="muted">
            {site.runtime.kind === "php"
              ? `PHP ${site.runtime.phpVersion}`
              : site.runtime.kind === "reverse-proxy"
                ? `${t("sites.proxyP1")} ${site.runtime.proxyTarget}`
                : site.runtime.kind === "static"
                  ? t("sites.static")
                  : site.runtime.kind}
          </Badge>
          {site.rewrite !== "none" && <Badge variant="outline">{site.rewrite}</Badge>}
          {site.db?.enabled && <Badge variant="outline">MySQL</Badge>}
        </div>

        <div className="mt-auto flex items-center justify-between border-t border-border pt-3">
          <code className="truncate rounded bg-card-2/70 px-2 py-1 font-mono text-[11px] text-secondary">
            {url.replace(/^https?:\/\//, "")}
          </code>
          <Button variant={running ? "secondary" : "default"} size="sm" onClick={toggle}>
            <Power className="h-3 w-3" />
            {running ? t("common.stop") : t("common.start")}
          </Button>
        </div>
      </Card>
    </motion.div>
  );
}
