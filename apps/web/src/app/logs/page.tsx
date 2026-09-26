"use client";

import * as React from "react";
import { toast } from "sonner";
import { Loader2, ScrollText } from "lucide-react";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { useSearchParams } from "next/navigation";
import { toastError, useServices, useSettings, useSites } from "@/lib/hooks";
import { isTauri } from "@/lib/backend";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { LogPane } from "@/components/shared/log-pane";
import { StatusLight } from "@/components/shared/status-light";
import { PageHeader } from "@/components/layout/app-shell";

export default function LogsPage() {
  // 静态导出时 useSearchParams 需要 Suspense 边界
  return (
    <React.Suspense fallback={null}>
      <LogsPageInner />
    </React.Suspense>
  );
}

function LogsPageInner() {
  const t = useT();
  const servicesQuery = useServices(4000);
  const { data: services, error: servicesError } = servicesQuery;
  const sitesQuery = useSites();
  const { data: sites, error: sitesError } = sitesQuery;
  const { data: settings } = useSettings();
  const searchParams = useSearchParams();
  const wanted = searchParams.get("service");
  const [selected, setSelected] = React.useState<string | null>(wanted);
  const [query, setQuery] = React.useState("");
  const [exporting, setExporting] = React.useState(false);
  const exportBusy = React.useRef(false);

  // 从 URL 预选（服务卡片上的「日志」按钮会带上 ?service=）
  React.useEffect(() => {
    if (wanted) setSelected(wanted);
  }, [wanted]);

  React.useEffect(() => {
    // 指定了目标服务时，默认项不能覆盖 URL；后续轮询也不重置手动选择。
    if (!wanted && !selected && services.length > 0) setSelected(services[0].id);
  }, [wanted, services, selected]);

  // Nginx / Apache 均按站点读取访问日志。
  const siteItems = React.useMemo(
    () =>
      sites.map((s) => ({
        id: `site:${s.id}`,
        label: s.name,
        state: s.status as never,
      })),
    [sites]
  );

  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return services;
    return services.filter(
      (s) => s.label.toLowerCase().includes(q) || s.id.toLowerCase().includes(q)
    );
  }, [services, query]);

  const filteredSites = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return siteItems;
    return siteItems.filter((s) => s.label.toLowerCase().includes(q));
  }, [siteItems, query]);

  const runningCount = services.filter((s) => s.state === "running").length;
  const selectedLabel = services.find((s) => s.id === selected)?.label
    ?? siteItems.find((s) => s.id === selected)?.label ?? selected;
  const exportFullLog = async () => {
    if (!selected || exportBusy.current) return;
    const source = selected;
    const label = selectedLabel;
    exportBusy.current = true;
    setExporting(true);
    try {
      const name = `${source.replace(/[^a-zA-Z0-9._-]/g, "_")}-${new Date().toISOString().slice(0, 10)}.log`;
      let path: string | null = name;
      if (isTauri) {
        const { save } = await import("@tauri-apps/plugin-dialog");
        path = await save({ defaultPath: name, filters: [{ name: "Log", extensions: ["log", "txt"] }] });
      }
      if (!path) return;
      const bytes = await api.exportLog(source, path);
      toast.success(t(isTauri ? "logs.exported" : "log.downloadStarted"), {
        description: `${label} · ${bytes.toLocaleString()} B · ${path}`,
      });
    } catch (e) {
      toastError(e);
    } finally {
      exportBusy.current = false;
      setExporting(false);
    }
  };

  return (
    <div className="flex min-h-full flex-col pb-4">
      <PageHeader
        title={t("logs.title")}
        subtitle={t("logs.subtitle")}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-[11.5px] text-faint">
              {servicesError ? t("logs.servicesFailed") : !servicesQuery.dataUpdatedAt ? t("common.loading") : `${runningCount}/${services.length} ${t("logs.runningCount")}`}
            </span>
            {/* 导出当前选中服务的完整日志文件 */}
            <Button
              variant="ghost"
              size="sm"
              disabled={!selected || exporting}
              onClick={() => void exportFullLog()}
            >
              {exporting && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
              {t("logs.export")}
            </Button>
          </div>
        }
      />
      <div className="grid flex-1 grid-cols-1 items-start gap-3 md:grid-cols-[240px_minmax(0,1fr)] md:items-stretch md:gap-4">
        {/* 左侧选择器 */}
        <Card className="flex h-60 min-h-0 flex-col p-2 md:h-full">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("logs.filterServices")}
            aria-label={t("logs.filterServices")}
            className="mb-2 h-7 w-full shrink-0 rounded-md border border-transparent bg-fill px-2.5 text-[11.5px] text-foreground transition-colors placeholder:text-faint focus:border-primary focus:outline-none"
          />
          <div className="min-h-0 flex-1 overflow-y-auto">
            <p className="px-2 py-1 text-[10.5px] font-medium uppercase tracking-wider text-faint/70">
              {t("logs.services")}
            </p>
            {servicesError ? (
              <div role="alert" className="px-2 py-1 text-[11px] text-error/80">
                <p>{t("logs.servicesFailed")}</p>
                <Button size="sm" variant="ghost" disabled={servicesQuery.isFetching} onClick={() => void servicesQuery.refetch()}>{t("log.refresh")}</Button>
              </div>
            ) : !servicesQuery.dataUpdatedAt ? (
              <p className="px-2 py-1 text-[11px] text-faint">{t("common.loading")}</p>
            ) : filtered.length === 0 ? (
              <p className="px-2 py-1 text-[11px] text-faint/50">{t("logs.none")}</p>
            ) : (
              filtered.map((item) => (
                <button
                  key={item.id}
                  aria-pressed={selected === item.id}
                  onClick={() => setSelected(item.id)}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-[12.5px] transition-colors",
                    selected === item.id
                      ? "bg-card-2 font-medium text-foreground"
                      : "text-muted hover:bg-fill hover:text-foreground"
                  )}
                >
                  <StatusLight state={item.state} size={6} />
                  <span className="truncate">{item.label}</span>
                  {item.state === "error" && (
                    <span className="ml-auto h-1.5 w-1.5 shrink-0 rounded-full bg-error" />
                  )}
                </button>
              ))
            )}
            {/* 站点访问日志 */}
            <p className="mt-2 px-2 py-1 text-[10.5px] font-medium uppercase tracking-wider text-faint/70">
              {t("logs.sites")}
            </p>
            {sitesError ? (
              <div role="alert" className="px-2 py-1 text-[11px] text-error/80">
                <p>{t("logs.sitesFailed")}</p>
                <Button size="sm" variant="ghost" disabled={sitesQuery.isFetching} onClick={() => void sitesQuery.refetch()}>{t("log.refresh")}</Button>
              </div>
            ) : !sitesQuery.dataUpdatedAt ? (
              <p className="px-2 py-1 text-[11px] text-faint">{t("common.loading")}</p>
            ) : filteredSites.length === 0 ? (
              <p className="px-2 py-1 text-[11px] text-faint/50">{t("logs.none")}</p>
            ) : (
              filteredSites.map((item) => (
                <button
                  key={item.id}
                  aria-pressed={selected === item.id}
                  onClick={() => setSelected(item.id)}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-[12.5px] transition-colors",
                    selected === item.id
                      ? "bg-card-2 font-medium text-foreground"
                      : "text-muted hover:bg-fill hover:text-foreground"
                  )}
                >
                  <StatusLight state={item.state} size={6} />
                  <span className="truncate">{item.label}</span>
                </button>
              ))
            )}
          </div>
        </Card>

        {/* 日志面板 */}
        <Card className="flex h-full min-h-0 min-w-0 flex-col p-3 sm:p-4">
          {selected ? (
            <>
              <div className="mb-2 flex items-center gap-2 text-[12px] text-faint">
                <ScrollText className="h-3.5 w-3.5 shrink-0" />
                <span className="min-w-0 break-all">{selectedLabel}</span>
              </div>
              <LogPane
                serviceId={selected}
                className="min-h-0 flex-1"
                height={520}
                emptyHint={t(selected.startsWith("site:") ? "logs.siteEmptyHint" : "logs.emptyHint")}
                tailLines={settings?.logTailLines ?? 500}
                defaultAutoRefresh={settings?.logAutoRefresh ?? true}
              />
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center text-sm text-faint">{t("logs.pickHint")}</div>
          )}
        </Card>
      </div>
    </div>
  );
}
