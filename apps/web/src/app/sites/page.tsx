"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import { Globe, Plus, ExternalLink, FolderOpen, Loader2, Power, Settings2, TerminalSquare, FolderSearch, Copy, AppWindow, Search, RefreshCw } from "lucide-react";
import type { Site } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useSites, useInvalidate, toastError, siteUrl, usePorts } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { StatusLight } from "@/components/shared/status-light";
import { CopyButton, EmptyState } from "@/components/shared/misc";
import { PageHeader } from "@/components/layout/app-shell";
import { SiteDetailSheet } from "@/components/sites/site-detail";
import { ProjectScannerDialog } from "@/components/sites/project-scanner";
import { SiteBulkActions } from "@/components/sites/site-bulk-actions";

export default function SitesPage() {
  const t = useT();
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const { data: sites, error, isFetching, dataUpdatedAt, refetch } = useSites();
  const [detailId, setDetailId] = React.useState<string | null>(null);
  const detail = sites.find((site) => site.id === detailId) ?? null;
  const [query, setQuery] = React.useState("");
  const [statusFilter, setStatusFilter] = React.useState("all");
  const [serverFilter, setServerFilter] = React.useState("all");
  const visibleSites = React.useMemo(() => {
    const search = query.trim().toLowerCase();
    return sites.filter((site) =>
      (statusFilter === "all" || site.status === statusFilter) &&
      (serverFilter === "all" || site.runtime.webServer === serverFilter) &&
      (!search || [site.name, ...site.domains, site.rootDir, site.runtime.phpVersion ?? "", site.runtime.proxyTarget ?? ""].some((value) => value.toLowerCase().includes(search)))
    ).sort((a, b) => b.updatedAt - a.updatedAt);
  }, [sites, query, statusFilter, serverFilter]);
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
            {/* 批量启停：站点多了以后一个个点开关很费事 */}
            <SiteBulkActions sites={visibleSites} />
            {/* 批量打开 / 复制全部地址：起一套站点后逐个点开太磨人 */}
            <BatchUrlActions sites={visibleSites} />
            <Button onClick={() => setWizardOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("sites.create")}
            </Button>
          </>
        }
      />

      <div className="mb-5 flex flex-wrap items-center gap-3">
        <div className="relative min-w-[180px] flex-1 sm:max-w-sm">
          <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-faint" />
          <Input aria-label={t("sites.search")} placeholder={t("sites.search")} value={query} onChange={(event) => setQuery(event.target.value)} className="pl-9" />
        </div>
        <Select value={statusFilter} onValueChange={setStatusFilter}>
          <SelectTrigger className="w-[140px]" aria-label={t("sites.filterStatus")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("sites.allStatuses")}</SelectItem>
            <SelectItem value="running">{t("state.running")}</SelectItem>
            <SelectItem value="stopped">{t("state.stopped")}</SelectItem>
            <SelectItem value="error">{t("state.error")}</SelectItem>
            <SelectItem value="unconfigured">{t("sites.unconfigured")}</SelectItem>
          </SelectContent>
        </Select>
        <Select value={serverFilter} onValueChange={setServerFilter}>
          <SelectTrigger className="w-[140px]" aria-label={t("sites.wizard.webServer")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("sites.allServers")}</SelectItem>
            <SelectItem value="nginx">Nginx</SelectItem>
            <SelectItem value="apache">Apache</SelectItem>
          </SelectContent>
        </Select>
        <span className="text-xs tabular-nums text-muted" aria-live="polite">{visibleSites.length} / {sites.length}</span>
      </div>
      {error && (
        <div role="alert" className="mb-4 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-error/20 bg-error/5 p-4">
          <span className="text-sm text-error">{t("sites.loadFailed")}</span>
          <Button variant="secondary" size="sm" disabled={isFetching} onClick={() => refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("sites.retry")}</Button>
        </div>
      )}
      {dataUpdatedAt === 0 && isFetching ? (
        <div role="status" className="flex min-h-[280px] items-center justify-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 animate-spin" />{t("common.loading")}</div>
      ) : error && sites.length === 0 ? null : sites.length === 0 ? (
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
      ) : visibleSites.length === 0 ? (
        <EmptyState icon={Search} title={t("sites.noMatches")} hint={t("sites.noMatchesHint")}
          action={<Button variant="secondary" onClick={() => { setQuery(""); setStatusFilter("all"); setServerFilter("all"); }}>{t("sites.clearFilters")}</Button>} />
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <AnimatePresence>
            {visibleSites
              .map((site) => (
                <SiteCard key={site.id} site={site} onOpenDetail={() => setDetailId(site.id)} />
              ))}
          </AnimatePresence>
        </div>
      )}

      <ProjectScannerDialog open={scanOpen} onOpenChange={setScanOpen} />

      <SiteDetailSheet site={detail} onClose={() => setDetailId(null)} />
    </div>
  );
}

function SiteCard({ site, onOpenDetail }: { site: Site; onOpenDetail: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const ports = usePorts();
  const url = siteUrl(site, ports);
  const running = site.status === "running";
  const [pending, setPending] = React.useState(false);

  const toggle = async () => {
    if (pending) return;
    setPending(true);
    try {
      if (running) await api.stopSite(site.id);
      else await api.startSite(site.id);
      invalidate("sites", "services", "hosts");
    } catch (e) {
      toastError(e);
    } finally {
      setPending(false);
    }
  };

  return (
    <motion.div layout initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, scale: 0.96 }}>
      <Card className="group flex flex-col gap-3 p-4 transition-all hover:border-border-strong">
        <div className="flex flex-col gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
              <Globe className={`h-4 w-4 ${running ? "text-running" : "text-faint"}`} strokeWidth={1.8} />
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <span className="truncate text-[13.5px] font-medium" title={site.name}>{site.name}</span>
                <StatusLight state={site.status} size={6} />
              </div>
              <span className="block truncate text-[11px] text-faint" title={site.domains.join(", ")}>{site.domains.join(", ")}</span>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-0.5">
            <CopyButton text={url} />
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("dashboard.openBrowser")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openInBrowser(url).catch(toastError)}>
                  <ExternalLink className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openBrowser")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("dashboard.openFolder")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
                  <FolderOpen className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openFolder")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("dashboard.openTerminal")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openTerminal(site.rootDir).catch(toastError)}>
                  <TerminalSquare className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openTerminal")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("sites.siteSettings")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={onOpenDetail}>
                  <Settings2 className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("sites.siteSettings")}</TooltipContent>
            </Tooltip>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant="outline">{site.runtime.webServer === "apache" ? "Apache" : "Nginx"}</Badge>
          {site.https && <Badge variant="info">HTTPS</Badge>}
          <Badge variant="muted" className="max-w-full break-all whitespace-normal">
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

        <div className="mt-auto flex items-center justify-between gap-3 border-t border-dashed border-separator pt-3">
          <code className="min-w-0 truncate rounded bg-card-2/70 px-2 py-1 font-mono text-[11px] text-secondary">
            {url.replace(/^https?:\/\//, "")}
          </code>
          <Button variant={running ? "secondary" : "default"} className="shrink-0" size="sm" disabled={pending} onClick={toggle}>
            {pending ? <Loader2 className="h-3 w-3 animate-spin" /> : <Power className="h-3 w-3" />}
            {pending ? t(running ? "common.stopping" : "common.starting") : running ? t("common.stop") : t("common.start")}
          </Button>
        </div>
      </Card>
    </motion.div>
  );
}


/* ============ 批量打开 / 复制全部站点地址 ============ */
function BatchUrlActions({ sites }: { sites: Site[] }) {
  const t = useT();
  const ports = usePorts();
  const [opening, setOpening] = React.useState(false);
  const urls = React.useMemo(
    () => sites.map((s) => siteUrl(s, ports)),
    [sites, ports]
  );

  const openAll = async () => {
    if (urls.length === 0 || opening) return;
    setOpening(true);
    let opened = 0;
    let failed = 0;
    // 逐个打开：浏览器会聚成一组标签页；间隔一点避免被弹窗拦截
    for (const u of urls) {
      try {
        await api.openInBrowser(u);
        opened++;
      } catch {
        failed++;
      }
      await new Promise((r) => setTimeout(r, 250));
    }
    setOpening(false);
    if (opened) toast.success(`${t("sites.openedAllP1")} ${opened} ${t("sites.openedAllP2")}`);
    if (failed) toast.error(`${t("sites.openFailed")} (${failed})`);
  };

  const copyAll = async () => {
    if (urls.length === 0) return;
    try {
      await navigator.clipboard.writeText(urls.join("\n"));
      toast.success(t("sites.copiedAll"));
    } catch {
      toast.error(t("sites.copyFailed"));
    }
  };

  if (sites.length === 0) return null;
  return (
    <>
      <Button variant="ghost" onClick={copyAll} title={t("sites.copyAllHint")}>
        <Copy className="h-3.5 w-3.5" /> {t("sites.copyAll")}
      </Button>
      <Button variant="ghost" disabled={opening} onClick={openAll} title={t("sites.openAllHint")}>
        <AppWindow className="h-3.5 w-3.5" /> {t("sites.openAll")}
      </Button>
    </>
  );
}
