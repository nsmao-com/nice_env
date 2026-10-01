"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import { Globe, Plus, ExternalLink, FolderOpen, Loader2, Power, Settings2, FolderSearch, Copy, AppWindow, Search, RefreshCw, Share2, Network, Star, X, FolderKanban, Pencil, Trash2 } from "lucide-react";
import type { Site, SiteGroup } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useSites, useInvalidate, toastError, siteUrl, useSettings } from "@/lib/hooks";
import { normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { SiteTerminalButton } from "@/components/sites/site-terminal";
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
import { SiteNetworkDialog } from "@/components/sites/site-network";
import { SiteShareDialog } from "@/components/shared/tunnel-panel";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/shared/misc";

type SiteSort = "recent" | "name" | "status" | "runtime";

const SITE_STATUS_ORDER: Record<Site["status"], number> = {
  running: 0,
  error: 1,
  stopped: 2,
  unconfigured: 3,
};

export default function SitesPage() {
  const t = useT();
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const { data: sites, error, isFetching, dataUpdatedAt, refetch } = useSites();
  const invalidate = useInvalidate();
  const settings = useSettings();
  const [favoriteIds, setFavoriteIds] = React.useState<Set<string>>(new Set());
  const [favoriteFilter, setFavoriteFilter] = React.useState("all");
  const [favoriteBusyId, setFavoriteBusyId] = React.useState<string | null>(null);
  const favoriteKey = JSON.stringify(settings.data?.favoriteSites ?? []);
  React.useEffect(() => {
    if (settings.data) setFavoriteIds(new Set(settings.data.favoriteSites ?? []));
  }, [favoriteKey]);
  const toggleFavorite = async (siteId: string) => {
    if (favoriteBusyId) return;
    const previous = new Set(favoriteIds);
    const next = new Set(favoriteIds);
    const favorite = !next.has(siteId);
    if (favorite) next.add(siteId); else next.delete(siteId);
    setFavoriteIds(next);
    setFavoriteBusyId(siteId);
    try {
      await api.setSetting("favoriteSites", [...next]);
      await invalidate("settings");
      toast.success(t(favorite ? "sites.favoriteAdded" : "sites.favoriteRemoved"));
    } catch (error) {
      setFavoriteIds(previous);
      toastError(error);
    } finally {
      setFavoriteBusyId(null);
    }
  };
  const [networkId, setNetworkId] = React.useState<string | null>(null);
  const networkSite = sites.find((site) => site.id === networkId);
  const [shareId, setShareId] = React.useState<string | null>(null);
  const sharedSite = sites.find((site) => site.id === shareId);
  const [detailId, setDetailId] = React.useState<string | null>(null);
  const detail = sites.find((site) => site.id === detailId) ?? null;
  const [query, setQuery] = React.useState("");
  const [statusFilter, setStatusFilter] = React.useState("all");
  const [serverFilter, setServerFilter] = React.useState("all");
  const [groupFilter, setGroupFilter] = React.useState("all");
  const [sortBy, setSortBy] = React.useState<SiteSort>("recent");
  const siteGroups = settings.data?.siteGroups ?? [];
  const groupAssignments = settings.data?.siteGroupAssignments ?? {};
  const groupNameById = React.useMemo(() => new Map(siteGroups.map((group) => [group.id, group.name])), [siteGroups]);
  const [groupsOpen, setGroupsOpen] = React.useState(false);
  const [groupBusyId, setGroupBusyId] = React.useState<string | null>(null);
  const updateSiteGroup = async (siteId: string, groupId: string) => {
    if (groupBusyId) return;
    const next = { ...groupAssignments };
    if (groupId === "__none__") delete next[siteId];
    else next[siteId] = groupId;
    setGroupBusyId(siteId);
    try {
      await api.setSetting("siteGroupAssignments", next);
      invalidate("settings");
      toast.success(t("sites.groupAssigned"));
    } catch (error) {
      toastError(error);
    } finally {
      setGroupBusyId(null);
    }
  };
  const visibleSites = React.useMemo(() => {
    const search = query.trim().toLowerCase();
    const filtered = sites.filter((site) =>
      (statusFilter === "all" || site.status === statusFilter) &&
      (serverFilter === "all" || site.runtime.webServer === serverFilter) &&
      (favoriteFilter !== "favorites" || favoriteIds.has(site.id)) &&
      (groupFilter === "all" || (groupFilter === "ungrouped" ? !groupAssignments[site.id] : groupAssignments[site.id] === groupFilter)) &&
      (!search || [site.name, ...site.domains, site.rootDir, site.runtime.phpVersion ?? "", site.runtime.proxyTarget ?? "", site.runtime.redirect?.target ?? "", groupNameById.get(groupAssignments[site.id] ?? "") ?? ""].some((value) => value.toLowerCase().includes(search)))
    );
    return filtered.sort((a, b) => {
      const favoriteOrder = Number(favoriteIds.has(b.id)) - Number(favoriteIds.has(a.id));
      if (favoriteOrder !== 0) return favoriteOrder;
      if (sortBy === "name") return a.name.localeCompare(b.name, "zh-CN", { sensitivity: "base" });
      if (sortBy === "status") return SITE_STATUS_ORDER[a.status] - SITE_STATUS_ORDER[b.status] || b.updatedAt - a.updatedAt;
      if (sortBy === "runtime") return a.runtime.kind.localeCompare(b.runtime.kind) || a.name.localeCompare(b.name, "zh-CN", { sensitivity: "base" });
      return b.updatedAt - a.updatedAt;
    });
  }, [sites, query, statusFilter, serverFilter, favoriteFilter, favoriteIds, groupFilter, groupAssignments, groupNameById, sortBy]);
  const hasFilters = !!query.trim() || statusFilter !== "all" || serverFilter !== "all" || favoriteFilter !== "all" || groupFilter !== "all" || sortBy !== "recent";
  const resetFilters = () => {
    setQuery("");
    setStatusFilter("all");
    setServerFilter("all");
    setFavoriteFilter("all");
    setGroupFilter("all");
    setSortBy("recent");
  };
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
            <Button variant="ghost" size="sm" onClick={() => void refetch()} disabled={isFetching} title={t("sites.refreshHint")} aria-label={t("sites.refresh")}>
              <RefreshCw className={isFetching ? "h-3.5 w-3.5 animate-spin" : "h-3.5 w-3.5"} />
              <span className="hidden sm:inline">{t("sites.refresh")}</span>
            </Button>
            {/* 已有项目的人多半不想手填表单，先给「扫一下」这条路 */}
            <Button variant="secondary" onClick={() => setScanOpen(true)}>
              <FolderSearch className="h-3.5 w-3.5" /> {t("scanner.scan")}
            </Button>
            {/* 批量启停：站点多了以后一个个点开关很费事 */}
            <SiteBulkActions sites={visibleSites} />
            {/* 批量打开 / 复制全部地址：起一套站点后逐个点开太磨人 */}
            <BatchUrlActions sites={visibleSites} />
            <Button variant="secondary" onClick={() => setWizardOpen(true, "redirect")}>{t("redirect.title")}</Button>
            <Button onClick={() => setWizardOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("sites.create")}
            </Button>
          </>
        }
      />

      <div className="mb-5 flex flex-wrap items-center gap-2.5 sm:gap-3">
        <div className="relative min-w-0 basis-full sm:min-w-[180px] sm:flex-1 sm:basis-auto sm:max-w-sm">
          <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-faint" />
          <Input aria-label={t("sites.search")} placeholder={t("sites.search")} value={query} onChange={(event) => setQuery(event.target.value)} className="pl-9" />
        </div>
        <Select value={statusFilter} onValueChange={setStatusFilter}>
          <SelectTrigger className="w-full sm:w-[140px]" aria-label={t("sites.filterStatus")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("sites.allStatuses")}</SelectItem>
            <SelectItem value="running">{t("state.running")}</SelectItem>
            <SelectItem value="stopped">{t("state.stopped")}</SelectItem>
            <SelectItem value="error">{t("state.error")}</SelectItem>
            <SelectItem value="unconfigured">{t("sites.unconfigured")}</SelectItem>
          </SelectContent>
        </Select>
        <Select value={serverFilter} onValueChange={setServerFilter}>
          <SelectTrigger className="w-full sm:w-[140px]" aria-label={t("sites.wizard.webServer")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("sites.allServers")}</SelectItem>
            <SelectItem value="nginx">Nginx</SelectItem>
            <SelectItem value="apache">Apache</SelectItem>
            <SelectItem value="caddy">Caddy</SelectItem>
          </SelectContent>
        </Select>
        <Select value={favoriteFilter} onValueChange={setFavoriteFilter}>
          <SelectTrigger className="w-full sm:w-[140px]" aria-label={t("sites.filterFavorites")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("sites.allFavorites")}</SelectItem>
            <SelectItem value="favorites">{t("sites.favoritesOnly")}</SelectItem>
          </SelectContent>
        </Select>
        <Select value={groupFilter} onValueChange={setGroupFilter} disabled={settings.isPending || !!settings.error}>
          <SelectTrigger className="w-full sm:w-[150px]" aria-label={t("sites.filterGroup")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("sites.allGroups")}</SelectItem>
            <SelectItem value="ungrouped">{t("sites.ungrouped")}</SelectItem>
            {siteGroups.map((group) => <SelectItem key={group.id} value={group.id}>{group.name}</SelectItem>)}
          </SelectContent>
        </Select>
        <Button variant="ghost" size="sm" className="shrink-0" aria-label={t("sites.manageGroups")} title={t("sites.manageGroups")} onClick={() => setGroupsOpen(true)}>
          <FolderKanban className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t("sites.manageGroups")}</span>
        </Button>
        <Select value={sortBy} onValueChange={(value) => setSortBy(value as SiteSort)}>
          <SelectTrigger className="w-full sm:w-[140px]" aria-label={t("sites.sortBy")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="recent">{t("sites.sortRecent")}</SelectItem>
            <SelectItem value="name">{t("sites.sortName")}</SelectItem>
            <SelectItem value="status">{t("sites.sortStatus")}</SelectItem>
            <SelectItem value="runtime">{t("sites.sortRuntime")}</SelectItem>
          </SelectContent>
        </Select>
        <div className="flex w-full min-w-0 items-center justify-between gap-2 sm:w-auto sm:justify-start">
          <span className="text-xs tabular-nums text-muted" aria-live="polite">{visibleSites.length} / {sites.length}</span>
          {hasFilters && <Button variant="ghost" size="sm" className="shrink-0" onClick={resetFilters}>
            <X className="h-3.5 w-3.5" /> {t("sites.clearFilters")}
          </Button>}
        </div>
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
        <EmptyState icon={favoriteFilter === "favorites" ? Star : Search} title={favoriteFilter === "favorites" ? t("sites.noFavorites") : t("sites.noMatches")} hint={favoriteFilter === "favorites" ? t("sites.noFavoritesHint") : t("sites.noMatchesHint")}
          action={<Button variant="secondary" onClick={resetFilters}>{t("sites.clearFilters")}</Button>} />
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <AnimatePresence>
            {visibleSites
              .map((site) => (
                <SiteCard key={site.id} site={site} groups={siteGroups} groupId={groupAssignments[site.id]} groupBusy={groupBusyId === site.id} favorite={favoriteIds.has(site.id)} favoriteBusy={favoriteBusyId !== null} onGroupChange={(groupId) => void updateSiteGroup(site.id, groupId)} onToggleFavorite={() => void toggleFavorite(site.id)} onOpenDetail={() => setDetailId(site.id)} onShare={() => setShareId(site.id)} onNetwork={() => setNetworkId(site.id)} />
              ))}
          </AnimatePresence>
        </div>
      )}

      <ProjectScannerDialog open={scanOpen} onOpenChange={setScanOpen} />

      <SiteGroupsDialog
        open={groupsOpen}
        groups={siteGroups}
        assignments={groupAssignments}
        onClose={() => setGroupsOpen(false)}
        onSaved={() => invalidate("settings")}
      />

      {networkId && <SiteNetworkDialog id={networkId} name={networkSite?.name ?? t("sites.share.unavailable")} onClose={() => setNetworkId(null)} />}
      {shareId && <SiteShareDialog siteId={shareId} name={sharedSite?.name ?? t("sites.share.unavailable")} onClose={() => setShareId(null)} />}
      <SiteDetailSheet site={detail} onClose={() => setDetailId(null)} />
    </div>
  );
}

function SiteCard({ site, groups, groupId, groupBusy, favorite, favoriteBusy, onGroupChange, onToggleFavorite, onOpenDetail, onShare, onNetwork }: { site: Site; groups: SiteGroup[]; groupId?: string; groupBusy: boolean; favorite: boolean; favoriteBusy: boolean; onGroupChange: (groupId: string) => void; onToggleFavorite: () => void; onOpenDetail: () => void; onShare: () => void; onNetwork: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const url = siteUrl(site);
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
          <div className="flex shrink-0 flex-wrap items-center gap-0.5">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t(favorite ? "sites.unfavorite" : "sites.favorite")} aria-pressed={favorite} variant="ghost" size="icon-sm" className={favorite ? "text-amber-500 hover:text-amber-600" : "text-faint hover:text-foreground"} disabled={favoriteBusy} onClick={onToggleFavorite}>
                  <Star className="h-3.5 w-3.5" fill={favorite ? "currentColor" : "none"} />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t(favorite ? "sites.unfavorite" : "sites.favorite")}</TooltipContent>
            </Tooltip>
            <CopyButton text={url} resolveText={() => api.siteAccessUrl(site.id)} />
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("lan.title")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={onNetwork}><Network className="h-3.5 w-3.5" /></Button>
              </TooltipTrigger>
              <TooltipContent>{t("lan.title")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("sites.share.title")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={onShare}>
                  <Share2 className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("sites.share.title")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("dashboard.openBrowser")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openSite(site.id).catch(toastError)}>
                  <ExternalLink className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openBrowser")}</TooltipContent>
            </Tooltip>
            {site.runtime.kind !== "redirect" && <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label={t("dashboard.openFolder")} variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
                  <FolderOpen className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("dashboard.openFolder")}</TooltipContent>
            </Tooltip>}
            {site.runtime.kind !== "redirect" && <SiteTerminalButton site={site} />}
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
          <Badge variant="outline">{site.runtime.webServer === "caddy" ? "Caddy" : site.runtime.webServer === "apache" ? "Apache" : "Nginx"}</Badge>
          {site.https && <Badge variant="info">HTTPS</Badge>}
          <Badge variant="muted" className="max-w-full break-all whitespace-normal">
            {site.runtime.kind === "php"
              ? `PHP ${site.runtime.phpVersion}`
              : site.runtime.kind === "reverse-proxy"
                ? `${t("sites.proxyP1")} ${site.runtime.proxyTarget}`
                : site.runtime.kind === "redirect"
                  ? t("redirect.title") + " · " + site.runtime.redirect?.status + " → " + (site.runtime.redirect?.target ?? "")
                : site.runtime.kind === "static"
                  ? t("sites.static")
                  : site.runtime.kind}
          </Badge>
          {site.rewrite !== "none" && <Badge variant="outline">{site.rewrite}</Badge>}
          {site.db?.enabled && <Badge variant="outline">MySQL</Badge>}
          {groups.length > 0 && <Select value={groupId ?? "__none__"} onValueChange={onGroupChange} disabled={groupBusy}>
            <SelectTrigger className="h-6 w-auto min-w-[108px] max-w-full gap-1 rounded-full border-dashed px-2 py-0.5 text-[10px] text-muted" aria-label={t("sites.filterGroup")}>
              <FolderKanban className="h-3 w-3 shrink-0" />
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__none__">{t("sites.groupNone")}</SelectItem>
              {groups.map((group) => <SelectItem key={group.id} value={group.id}>{group.name}</SelectItem>)}
            </SelectContent>
          </Select>}
        </div>

        <div className="mt-auto flex items-center justify-between gap-3 border-t border-dashed border-separator pt-3">
          <code className="min-w-0 truncate rounded bg-card-2/70 px-2 py-1 font-mono text-[11px] text-secondary">
            {url ? url.replace(/^https?:\/\//, "") : t("sites.addressPending")}
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

function SiteGroupsDialog({ open, groups, assignments, onClose, onSaved }: {
  open: boolean;
  groups: SiteGroup[];
  assignments: Record<string, string>;
  onClose: () => void;
  onSaved: () => void;
}) {
  const t = useT();
  const [newName, setNewName] = React.useState("");
  const [editingId, setEditingId] = React.useState<string | null>(null);
  const [editingName, setEditingName] = React.useState("");
  const [deleteTarget, setDeleteTarget] = React.useState<SiteGroup | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (open) {
      setNewName("");
      setEditingId(null);
      setEditingName("");
      setDeleteTarget(null);
      setError(null);
    }
  }, [open]);

  const duplicate = (name: string, except?: string) => groups.some((group) => group.id !== except && group.name.trim().toLocaleLowerCase() === name.trim().toLocaleLowerCase());
  const persistGroups = async (next: SiteGroup[]) => {
    await api.setSetting("siteGroups", next);
    onSaved();
  };
  const add = async (event: React.FormEvent) => {
    event.preventDefault();
    const name = newName.trim();
    if (!name) { setError(t("sites.groupRequired")); return; }
    if (duplicate(name)) { setError(t("sites.groupDuplicate")); return; }
    setBusy(true); setError(null);
    try {
      const id = `site-group-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      await persistGroups([...groups, { id, name }]);
      setNewName("");
      toast.success(t("sites.groupSaved"));
    } catch (e) { setError(normalizeGroupError(e)); }
    finally { setBusy(false); }
  };
  const rename = async (group: SiteGroup) => {
    const name = editingName.trim();
    if (!name) { setError(t("sites.groupRequired")); return; }
    if (duplicate(name, group.id)) { setError(t("sites.groupDuplicate")); return; }
    setBusy(true); setError(null);
    try {
      await persistGroups(groups.map((item) => item.id === group.id ? { ...item, name } : item));
      setEditingId(null); setEditingName("");
      toast.success(t("sites.groupSaved"));
    } catch (e) { setError(normalizeGroupError(e)); }
    finally { setBusy(false); }
  };
  const remove = async () => {
    if (!deleteTarget) return;
    setBusy(true); setError(null);
    try {
      const nextAssignments = Object.fromEntries(Object.entries(assignments).filter(([, id]) => id !== deleteTarget.id));
      await api.setSetting("siteGroups", groups.filter((group) => group.id !== deleteTarget.id));
      await api.setSetting("siteGroupAssignments", nextAssignments);
      onSaved();
      setDeleteTarget(null);
      toast.success(t("sites.groupDeleted"));
    } catch (e) { setError(normalizeGroupError(e)); }
    finally { setBusy(false); }
  };

  return <>
    <Dialog open={open} onOpenChange={(value) => { if (!value && !busy) onClose(); }}>
      <DialogContent className="flex max-h-[calc(100dvh-1.5rem)] max-w-lg flex-col overflow-hidden p-4 sm:p-6">
        <DialogHeader className="shrink-0 pr-7">
          <DialogTitle>{t("sites.groupManagerTitle")}</DialogTitle>
          <DialogDescription>{t("sites.groupManagerHint")}</DialogDescription>
        </DialogHeader>
        <div className="min-h-0 space-y-4 overflow-y-auto">
          <form className="flex flex-wrap items-end gap-2 border-b border-dashed border-separator pb-4" onSubmit={add}>
            <label className="min-w-0 flex-1 space-y-1.5">
              <span className="text-xs font-medium">{t("sites.groupName")}</span>
              <Input value={newName} maxLength={80} disabled={busy} placeholder={t("sites.groupNamePlaceholder")} onChange={(event) => { setNewName(event.target.value); setError(null); }} />
            </label>
            <Button type="submit" disabled={busy || !newName.trim()}><Plus className="h-3.5 w-3.5" />{t("sites.groupAdd")}</Button>
          </form>
          {error && <p role="alert" className="break-words rounded-lg bg-error-soft p-2.5 text-xs text-error">{error}</p>}
          <div className="space-y-2">
            {groups.length === 0 ? <p className="rounded-lg border border-dashed border-border p-4 text-center text-xs text-muted">{t("sites.ungrouped")}</p> : groups.map((group) => (
              <div key={group.id} className="flex flex-wrap items-center gap-2 rounded-lg border border-border/70 bg-card-2/20 p-2.5">
                {editingId === group.id ? <Input className="h-8 min-w-0 flex-1" value={editingName} maxLength={80} disabled={busy} autoFocus onChange={(event) => { setEditingName(event.target.value); setError(null); }} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); void rename(group); } if (event.key === "Escape") setEditingId(null); }} /> : <span className="min-w-0 flex-1 truncate text-sm font-medium">{group.name}</span>}
                <span className="text-[11px] text-faint">{Object.values(assignments).filter((id) => id === group.id).length}</span>
                {editingId === group.id ? <Button type="button" size="sm" disabled={busy || !editingName.trim()} onClick={() => void rename(group)}>{t("common.save")}</Button> : <Button type="button" size="icon-sm" variant="ghost" disabled={busy} aria-label={`${t("sites.groupRename")} ${group.name}`} onClick={() => { setEditingId(group.id); setEditingName(group.name); setError(null); }}><Pencil className="h-3.5 w-3.5" /></Button>}
                <Button type="button" size="icon-sm" variant="ghost" className="text-error hover:text-error" disabled={busy} aria-label={`${t("sites.groupDelete")} ${group.name}`} onClick={() => setDeleteTarget(group)}><Trash2 className="h-3.5 w-3.5" /></Button>
              </div>
            ))}
          </div>
        </div>
        <DialogFooter className="shrink-0"><Button variant="secondary" disabled={busy} onClick={onClose}>{t("common.close")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={!!deleteTarget} onOpenChange={(value) => { if (!value && !busy) setDeleteTarget(null); }} title={t("sites.groupDelete")} description={t("sites.groupDeleteConfirm")} confirmText={t("sites.groupDelete")} danger loading={busy} onConfirm={() => void remove()}>
      {deleteTarget && <p className="rounded-lg bg-fill p-3 text-sm font-medium break-words">{deleteTarget.name}</p>}
    </ConfirmDialog>
  </>;
}

function normalizeGroupError(error: unknown) {
  return normalizeError(error).message;
}


/* ============ 批量打开 / 复制全部站点地址 ============ */
function BatchUrlActions({ sites }: { sites: Site[] }) {
  const t = useT();
  const [opening, setOpening] = React.useState(false);
  const busyRef = React.useRef(false);

  const openAll = async () => {
    if (sites.length === 0 || busyRef.current) return;
    busyRef.current = true;
    setOpening(true);
    let opened = 0;
    const failed: string[] = [];
    // 逐个打开：浏览器会聚成一组标签页；间隔一点避免被弹窗拦截
    for (const site of sites) {
      try {
        await api.openSite(site.id);
        opened++;
      } catch {
        failed.push(site.name);
      }
      await new Promise((r) => setTimeout(r, 250));
    }
    setOpening(false);
    busyRef.current = false;
    if (opened) toast.success(`${t("sites.openedAllP1")} ${opened} ${t("sites.openedAllP2")}`);
    if (failed.length) toast.error(`${t("sites.addressUnavailable")} (${failed.length})`, { description: failed.join("、"), classNames: { description: "line-clamp-2 [overflow-wrap:anywhere]" } });
  };

  const copyAll = async () => {
    if (sites.length === 0 || busyRef.current) return;
    busyRef.current = true;
    setOpening(true);
    try {
      const urls: string[] = [];
      const failed: string[] = [];
      for (const site of sites) {
        try { urls.push(await api.siteAccessUrl(site.id)); }
        catch { failed.push(site.name); }
      }
      if (urls.length) {
        await navigator.clipboard.writeText(urls.join("\n"));
        toast.success(t("sites.copiedCount").replace("{count}", String(urls.length)));
      }
      if (failed.length) toast.error(`${t("sites.addressUnavailable")} (${failed.length})`, { description: failed.join("、"), classNames: { description: "line-clamp-2 [overflow-wrap:anywhere]" } });
    } catch {
      toast.error(t("sites.copyFailed"));
    } finally {
      busyRef.current = false;
      setOpening(false);
    }
  };

  if (sites.length === 0) return null;
  return (
    <>
      <Button variant="ghost" disabled={opening} onClick={copyAll} title={t("sites.copyAllHint")}>
        <Copy className="h-3.5 w-3.5" /> {t("sites.copyAll")}
      </Button>
      <Button variant="ghost" disabled={opening} onClick={openAll} title={t("sites.openAllHint")}>
        <AppWindow className="h-3.5 w-3.5" /> {t("sites.openAll")}
      </Button>
    </>
  );
}
