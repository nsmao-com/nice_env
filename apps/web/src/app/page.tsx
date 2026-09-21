"use client";

import * as React from "react";
import Link from "next/link";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  Rocket,
  Square,
  Globe,
  ExternalLink,
  FolderOpen,
  TerminalSquare,
  AlertTriangle,
  ChevronRight,
  Plus,
  Activity,
  CheckCircle2,
  Layers,
  LayoutGrid,
  List as ListIcon,
} from "lucide-react";
import { useUI, useT } from "@/lib/store";
import {
  useServices,
  useSites,
  useSystemStats,
  useInvalidate,
  toastError,
  toastPortConflict,
  siteUrl,
  usePorts,
  useStacks,
} from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ServiceCard } from "@/components/shared/service-card";
import { ServiceRow } from "@/components/shared/service-row";
import { StatusLight } from "@/components/shared/status-light";
import { CopyButton, EmptyState, SectionHeader, Sparkline, ConfirmDialog } from "@/components/shared/misc";
import { PageHeader } from "@/components/layout/app-shell";
import { HealthCard } from "@/components/shared/health-card";

export default function DashboardPage() {
  const t = useT();
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const { data: services } = useServices();
  const { data: sites } = useSites();
  const { data: stats } = useSystemStats(2000);
  const invalidate = useInvalidate();

  const [stackBusy, setStackBusy] = React.useState(false);
  const [confirmStopAll, setConfirmStopAll] = React.useState(false);
  const { data: stacks } = useStacks();
  const runningCount = services.filter((s) => s.state === "running").length;
  const anyRunning = runningCount > 0;

  /** 一键启动：优先用「用户保存的服务栈」（第一个 = 最常用的），没有则回落到内置 LNMP 顺序 */
  const startStack = async () => {
    setStackBusy(true);
    const chosen = stacks[0];
    if (chosen) {
      try {
        const report = await api.startStack(chosen.id);
        const failed = report.failed.length;
        if (failed > 0) {
          toast.warning(t("dashboard.stackPartial"), {
            description: report.failed.map((f) => `${f.serviceId}: ${f.error.message}`).join("\n"),
            duration: 9000,
          });
        } else {
          toast.success(t("dashboard.stackStarted"), {
            description: `${chosen.name} · ${report.started.length + report.alreadyRunning.length} ${t("stack.rptStarted")}`,
          });
        }
      } catch (e) {
        if (!toastPortConflict(e, { onResolved: () => void startStack() })) toastError(e);
      } finally {
        setStackBusy(false);
        invalidate("services", "stacks");
      }
      return;
    }
    // 还没有自定义栈：按内置顺序把已装服务起起来
    const ids = services
      .filter((s) => s.id === "nginx" || s.id === "redis" || s.id.startsWith("php@") || s.id.startsWith("mysql@"))
      .sort((a, b) => {
        const order = (id: string) => (id === "nginx" ? 3 : id.startsWith("php@") ? 2 : id.startsWith("mysql@") ? 1 : 0);
        return order(a.id) - order(b.id);
      })
      .map((s) => s.id);
    toast.promise(
      (async () => {
        for (const id of ids) {
          try {
            await api.startService(id);
          } catch {
            /* 单个失败继续，卡片上会看到状态 */
          }
        }
        invalidate("services");
      })(),
      { loading: t("dashboard.startingStack"), success: t("dashboard.stackStarted"), error: t("dashboard.someFailed") }
    ).unwrap().finally(() => setStackBusy(false));
  };

  const stopAll = async () => {
    setStackBusy(true);
    try {
      for (const s of services) {
        if (s.state === "running" || s.state === "starting") {
          await api.stopService(s.id).catch(() => undefined);
        }
      }
      toast.success(t("dashboard.allStopped"));
      setConfirmStopAll(false);
    } finally {
      setStackBusy(false);
      invalidate("services");
    }
  };

  const view = useUI((st) => st.serviceView);
  const setView = useUI((st) => st.setServiceView);
  const ports = usePorts();
  const recentSites = React.useMemo(
    () => [...sites].sort((a, b) => b.updatedAt - a.updatedAt).slice(0, 8),
    [sites]
  );

  return (
    <div className="pb-8">
      <PageHeader
        title={t("dash.title")}
        subtitle={t("dash.subtitle")}
        actions={
          <>
            {anyRunning ? (
              <Button
                variant="secondary"
                onClick={() => setConfirmStopAll(true)}
                disabled={stackBusy}
                title={t("dash.stopAllHint")}
              >
                <Square className="h-3.5 w-3.5" />

      {/* 环境体检：所有检查项的聚合入口，放在最上面 */}
      <section className="mb-5">
        <HealthCard />
      </section>
 {t("dash.stopAll")}
              </Button>
            ) : null}
            <Button onClick={startStack} disabled={stackBusy} title={t("dash.quickStartHint")}>
              <Rocket className="h-3.5 w-3.5" />{" "}
              {stacks[0] ? `${t("dash.startStack")}「${stacks[0].name}」` : t("dash.quickStart")}
            </Button>
            <Button variant="ghost" size="sm" asChild title={t("stack.title")}>
              <Link href="/stacks">
                <Layers className="h-3.5 w-3.5" />
              </Link>
            </Button>
          </>
        }
      />

      {services.length === 0 && sites.length === 0 ? (
        /* 首次进入：漂亮空状态 */
        <EmptyState
          icon={Globe}
          title={t("dash.noSites")}
          hint={t("dash.noSitesHint")}
          action={
            <Button size="lg" onClick={() => setWizardOpen(true)}>
              <Plus className="h-4 w-4" /> {t("dash.noSites")}
            </Button>
          }
          className="min-h-[420px]"
        />
      ) : (
        <div className="flex flex-col gap-6">
          {/* 服务健康矩阵 */}
          <section className="flex flex-col gap-3">
            <SectionHeader
              title={t("dash.health")}
              hint={`${runningCount}/${services.length} ${t("dash.runningCount")}`}
              actions={
                <div className="flex items-center gap-2">
                  {/* 卡片 / 列表切换（Apple 分段控件） */}
                  <Tabs
                    value={view}
                    onValueChange={(v) => setView(v as "card" | "list")}
                  >
                    <TabsList>
                      <TabsTrigger value="card" title={t("view.card")}>
                        <LayoutGrid className="h-3.5 w-3.5" />
                        <span className="hidden sm:inline">{t("view.card")}</span>
                      </TabsTrigger>
                      <TabsTrigger value="list" title={t("view.list")}>
                        <ListIcon className="h-3.5 w-3.5" />
                        <span className="hidden sm:inline">{t("view.list")}</span>
                      </TabsTrigger>
                    </TabsList>
                  </Tabs>
                  <Button variant="ghost" size="sm" asChild>
                    <Link href="/packages">
                      {t("dashboard.managePackages")} <ChevronRight className="h-3.5 w-3.5" />
                    </Link>
                  </Button>
                </div>
              }
            />
            {services.length === 0 ? (
              <EmptyState
                icon={Rocket}
                title={t("dashboard.noPackages")}
                hint={t("dashboard.stackGo")}
                action={
                  <Button asChild>
                    <Link href="/packages">{t("dashboard.browsePackages")}</Link>
                  </Button>
                }
              />
            ) : (
              view === "card" ? (
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
                  <AnimatePresence>
                    {services.map((s) => (
                      <ServiceCard key={s.id} service={s} />
                    ))}
                  </AnimatePresence>
                </div>
              ) : (
                <div className="flex flex-col gap-1.5">
                  <AnimatePresence>
                    {services.map((s) => (
                      <ServiceRow key={s.id} service={s} />
                    ))}
                  </AnimatePresence>
                </div>
              )
            )}
          </section>

          <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
            {/* 最近站点 */}
            <section className="flex flex-col gap-3 xl:col-span-2">
              <SectionHeader
                title={t("dash.recentSites")}
                actions={
                  <Button variant="ghost" size="sm" asChild>
                    <Link href="/sites">
                      {t("dashboard.allSites")} <ChevronRight className="h-3.5 w-3.5" />
                    </Link>
                  </Button>
                }
              />
              {recentSites.length === 0 ? (
                <EmptyState
                  icon={Globe}
                  title={t("sites.empty")}
                  hint={t("sites.emptyHint")}
                  action={
                    <Button onClick={() => setWizardOpen(true)}>
                      <Plus className="h-4 w-4" /> {t("sites.create")}
                    </Button>
                  }
                />
              ) : (
                <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
                  <AnimatePresence>
                    {recentSites.map((site) => {
                      const url = siteUrl(site, ports.http, ports.https);
                      return (
                        <motion.div key={site.id} layout initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, scale: 0.97 }}>
                          <Card className="group p-4 transition-colors hover:border-border-strong">
                            <div className="flex items-center justify-between gap-2">
                              <div className="flex items-center gap-2">
                                <StatusLight state={site.status === "unconfigured" ? "unknown" : site.status} size={7} />
                                <span className="text-[13.5px] font-medium">{site.name}</span>
                              </div>
                              <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
                                <CopyButton text={url} />
                                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" title={t("dashboard.openBrowser")} onClick={() => api.openInBrowser(url).catch(toastError)}>
                                  <ExternalLink className="h-3.5 w-3.5" />
                                </Button>
                                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" title={t("dashboard.openFolder")} onClick={() => api.openInFolder(site.rootDir).catch(toastError)}>
                                  <FolderOpen className="h-3.5 w-3.5" />
                                </Button>
                                <Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" title={t("dashboard.openTerminal")} onClick={() => api.openTerminal(site.rootDir).catch(toastError)}>
                                  <TerminalSquare className="h-3.5 w-3.5" />
                                </Button>
                              </div>
                            </div>
                            <div className="mt-2 flex items-center justify-between gap-2">
                              <a
                                className="truncate text-xs text-primary/90 hover:underline"
                                onClick={(e) => {
                                  e.preventDefault();
                                  api.openInBrowser(url).catch(toastError);
                                }}
                                href={url}
                              >
                                {url}
                              </a>
                              <span className="shrink-0 text-[10.5px] text-faint">
                                {site.runtime.kind === "php"
                                  ? `PHP ${site.runtime.phpVersion}`
                                  : site.runtime.kind === "reverse-proxy"
                                    ? `⇄ ${site.runtime.proxyTarget}`
                                    : site.runtime.kind}
                              </span>
                            </div>
                          </Card>
                        </motion.div>
                      );
                    })}
                  </AnimatePresence>
                </div>
              )}
            </section>

            {/* 右列：异常 + 资源 */}
            <div className="flex flex-col gap-6">
              <AnomalyCard />
              <Card className="p-4">
                <CardHeader className="p-0 pb-2">
                  <CardTitle className="flex items-center gap-2 text-[13px]">
                    <Activity className="h-3.5 w-3.5 text-primary" /> {t("dash.resources")}
                  </CardTitle>
                </CardHeader>
                <CardContent className="p-0">
                  <div className="flex flex-col gap-3">
                    <ResourceRow
                      label="CPU"
                      pct={stats?.cpuPercent ?? 0}
                      display={`${(stats?.cpuPercent ?? 0).toFixed(0)}%`}
                      data={stats?.history.map((h) => h.cpu) ?? []}
                    />
                    <ResourceRow
                      label={t("dashboard.mem")}
                      pct={stats ? (stats.memUsedMb / stats.memTotalMb) * 100 : 0}
                      display={`${((stats?.memUsedMb ?? 0) / 1024).toFixed(1)} / ${((stats?.memTotalMb ?? 0) / 1024).toFixed(0)} GB`}
                      data={stats?.history.map((h) => h.mem) ?? []}
                      stroke="var(--info)"
                    />
                    <div className="flex items-center justify-between text-[11px] text-faint">
                      <span>{t("dash.diskFree")} {(stats?.diskFreeGb ?? 0).toFixed(0)} GB</span>
                      <span>{t("dash.diskTotal")} {(stats?.diskTotalGb ?? 0).toFixed(0)} GB</span>
                    </div>
                  </div>
                </CardContent>
              </Card>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmStopAll}
        onOpenChange={setConfirmStopAll}
        title={t("confirm.stopAll")}
        description={t("confirm.stopAllDesc").replace(
          "{count}",
          String(services.filter((s) => s.state === "running" || s.state === "starting").length)
        )}
        confirmText={t("dash.stopAll")}
        danger
        loading={stackBusy}
        onConfirm={stopAll}
      />
    </div>
  );
}

function ResourceRow({
  label,
  pct,
  display,
  data,
  stroke = "var(--primary)",
}: {
  label: string;
  pct: number;
  display: string;
  data: number[];
  stroke?: string;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <div className="flex w-24 flex-col gap-0.5">
        <span className="text-[11px] font-medium text-secondary">{label}</span>
        <span className="text-[11px] tabular text-faint">{display}</span>
      </div>
      <Sparkline data={data} width={180} height={40} stroke={stroke} className="flex-1" />
      <span
        className={`w-9 text-right text-[11px] tabular ${pct > 85 ? "text-error" : pct > 60 ? "text-warn" : "text-running"}`}
      >
        {pct.toFixed(0)}%
      </span>
    </div>
  );
}

/** 异常卡片：端口冲突 / 证书过期 / 磁盘不足 / 上次失败 */
function AnomalyCard() {
  const t = useT();
  const { data: services } = useServices(5000);
  const { data: stats } = useSystemStats(10000);
  const { data: certs } = useCertsSafe();
  const [issues, setIssues] = React.useState<
    { kind: string; title: string; hint?: string }[]
  >([]);

  React.useEffect(() => {
    let alive = true;
    (async () => {
      const found: { kind: string; title: string; hint?: string }[] = [];
      /* 端口冲突：后端按当前端口方案全量核对，逐端口输出占用者 */
      try {
        const rows = await api.scanPorts();
        for (const r of rows.filter((r) => r.verdict === "conflict")) {
          found.push({
            kind: "port",
            title: `${t("dash.portConflict")}：${r.port} (${r.label}) → ${r.processName ?? t("dashboard.unknownProcess")}`,
            hint: t("dashboard.portScanHint"),
          });
        }
      } catch {
        /* ignore */
      }
      /* 证书 30 天内到期 */
      const soon = Date.now() + 30 * 86400_000;
      for (const c of certs) {
        if (c.kind === "site" && c.notAfter < soon && c.notAfter > Date.now()) {
          found.push({
            kind: "cert",
            title: `${t("dash.certExpiring")}：${c.subject}`,
            hint: t("dash.tlsHint"),
          });
        }
      }
      /* 磁盘 */
      if (stats && stats.diskFreeGb < 10) {
        found.push({
          kind: "disk",
          title: `${t("dash.diskLow")}：剩余 ${stats.diskFreeGb.toFixed(1)} GB`,
          hint: t("dashboard.dataDirHint"),
        });
      }
      /* 上次启动失败 */
      for (const s of services) {
        if (s.state === "error" && s.lastError) {
          found.push({
            kind: "fail",
            title: `${t("dash.lastFailure")}：${s.label}`,
            hint: s.lastError.hint ?? s.lastError.message,
          });
        }
      }
      if (alive) setIssues(found);
    })();
    return () => {
      alive = false;
    };
  }, [services, certs, stats, t]);

  return (
    <Card className="p-4">
      <CardHeader className="p-0 pb-2">
        <CardTitle className="flex items-center gap-2 text-[13px]">
          <AlertTriangle className={`h-3.5 w-3.5 ${issues.length ? "text-warn" : "text-running"}`} />
          {t("dash.anomalies")}
        </CardTitle>
        <CardDescription className="text-[11px]">
          {issues.length ? `${issues.length} ${t("dash.issuesCount")}` : t("dash.noAnomaly")}
        </CardDescription>
      </CardHeader>
      <CardContent className="p-0">
        {issues.length === 0 ? (
          <div className="flex items-center gap-2 rounded-lg border border-running/20 bg-running-soft px-3 py-2.5 text-[12px] text-running">
            <CheckCircle2 className="h-3.5 w-3.5" /> {t("dash.noAnomaly")}
          </div>
        ) : (
          <div className="flex flex-col gap-2">
            {issues.map((issue, i) => (
              <div key={i} className="rounded-lg border border-warn/25 bg-warn/10 px-3 py-2">
                <p className="text-[12px] font-medium text-warn">{issue.title}</p>
                {issue.hint && <p className="mt-0.5 text-[11px] text-warn/70">{issue.hint}</p>}
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function useCertsSafe() {
  const [state, setState] = React.useState<{ data: { kind: string; subject: string; notAfter: number }[] }>({
    data: [],
  });
  React.useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .listCerts()
        .then((data) => alive && setState({ data }))
        .catch(() => undefined);
    load();
    const id = setInterval(load, 15000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);
  return state;
}
