"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  Globe,
  ExternalLink,
  LayoutDashboard,
  Boxes,
  Database,
  ShieldCheck,
  Waypoints,
  Wrench,
  ScrollText,
  Settings,
  Rocket,
  Square,
  Plus,
  Layers,
  RotateCw,
  FolderSearch,
  Activity,
  Stethoscope,
  FileCog,
  Loader2,
  X,
} from "lucide-react";
import { CommandDialog, CommandGroup, CommandInput, CommandItem, CommandList, CommandEmpty } from "@/components/ui/command";
import { useUI, useT } from "@/lib/store";
import { useServices, useSites, useStacks, toastError, useQuickServiceActions, serviceHasProcess } from "@/lib/hooks";
import * as api from "@/lib/api";
import { StatusLight } from "@/components/shared/status-light";
import { BulkResult, BulkTargetList } from "@/components/shared/bulk-actions";
import { ConfirmDialog } from "@/components/shared/misc";
import { Button } from "@/components/ui/button";

const PAGES: { href: string; icon: typeof LayoutDashboard; labelKey: string }[] = [
  { href: "/", icon: LayoutDashboard, labelKey: "cmd.page.dashboard" },
  { href: "/sites", icon: Globe, labelKey: "cmd.page.sites" },
  { href: "/packages", icon: Boxes, labelKey: "cmd.page.packages" },
  { href: "/stacks", icon: Layers, labelKey: "cmd.page.stacks" },
  { href: "/databases", icon: Database, labelKey: "cmd.page.databases" },
  { href: "/tls", icon: ShieldCheck, labelKey: "cmd.page.tls" },
  { href: "/proxy", icon: Waypoints, labelKey: "cmd.page.proxy" },
  { href: "/configuration", icon: FileCog, labelKey: "nav.configuration" },
  { href: "/rewrites", icon: Wrench, labelKey: "nav.rewrites" },
  { href: "/network", icon: Wrench, labelKey: "nav.network" },
  { href: "/environment", icon: Wrench, labelKey: "nav.environment" },
  { href: "/tasks", icon: Wrench, labelKey: "nav.tasks" },
  { href: "/tunnels", icon: Wrench, labelKey: "nav.tunnels" },
  { href: "/models", icon: Wrench, labelKey: "nav.models" },
  { href: "/backups", icon: Wrench, labelKey: "nav.backups" },
  { href: "/diagnostics", icon: Stethoscope, labelKey: "nav.diagnostics" },
  { href: "/tools", icon: Wrench, labelKey: "cmd.page.tools" },
  { href: "/logs", icon: ScrollText, labelKey: "cmd.page.logs" },
  { href: "/settings", icon: Settings, labelKey: "cmd.page.settings" },
];

export function CommandPalette() {
  const open = useUI((s) => s.commandOpen);
  const setOpen = useUI((s) => s.setCommandOpen);
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const t = useT();
  const router = useRouter();
  const serviceQuery = useServices(open ? 2000 : 0);
  const siteQuery = useSites();
  const stackQuery = useStacks();
  const services = serviceQuery.data;
  const sites = siteQuery.data;
  const stacks = stackQuery.data;
  const servicesReady = serviceQuery.dataUpdatedAt > 0 && !serviceQuery.error;
  const stacksReady = stackQuery.dataUpdatedAt > 0 && !stackQuery.error;
  const sitesReady = siteQuery.dataUpdatedAt > 0 && !siteQuery.error;
  const readError = serviceQuery.error || siteQuery.error || stackQuery.error;
  const loading = !serviceQuery.dataUpdatedAt || !siteQuery.dataUpdatedAt || !stackQuery.dataUpdatedAt;
  const refreshing = serviceQuery.isFetching || siteQuery.isFetching || stackQuery.isFetching;
  const retryRead = () => { void Promise.all([serviceQuery.refetch(), siteQuery.refetch(), stackQuery.refetch()]); };
  const [confirmStopAll, setConfirmStopAll] = React.useState(false);
  const quick = useQuickServiceActions(services, stacks);
  const quickStackId = useUI((s) => s.quickStackId);
  const selectedStack = stacks.find((stack) => stack.id === quickStackId) ?? stacks[0];
  const busy = quick.busy;
  const failure = quick.serviceFailure;
  const failedService = failure && services.find((service) => service.id === failure.service.id);
  const retryUnavailable = quick.serviceTargetChanged || !failedService || ["unknown", "starting", "stopping"].includes(failedService.state)
    || (failure?.action !== "stop" && failedService.missingRequires.length > 0);

  React.useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen(!open);
      }
    };
    document.addEventListener("keydown", down);
    return () => document.removeEventListener("keydown", down);
  }, [open, setOpen]);

  const run = (fn: () => unknown | Promise<unknown>) => {
    setOpen(false);
    Promise.resolve(fn()).catch((e) => toastError(e));
  };

  const startStack = () => quick.start(selectedStack);
  const stopAll = async () => {
    setOpen(false);
    if (await quick.prepareStop()) setConfirmStopAll(true);
  };
  const doStopAll = async () => {
    const report = await quick.stop(quick.stopReport?.failed.map((f) => f.serviceId));
    if (report && !report.failed.length) setConfirmStopAll(false);
  };

  return (
    <>
    <CommandDialog open={open} onOpenChange={setOpen}>
      <CommandInput placeholder={t("cmd.placeholder")} />
      {failure && (
        <div role="alert" className="mx-3 mt-2 max-h-[40dvh] shrink-0 overflow-y-auto rounded-lg border border-error/25 bg-error-soft p-3 text-xs [overflow-wrap:anywhere]">
          <div className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <p className="font-medium text-error">{t("cmd.serviceActionFailed")}</p>
              <p className="mt-1 text-muted">{failure.service.label}{failure.service.version ? ` · ${failure.service.version}` : ""} · {t(`common.${failure.action}`)}</p>
            </div>
            <Button variant="ghost" size="icon-sm" className="shrink-0" aria-label={t("cmd.dismissError")} title={t("cmd.dismissError")} disabled={busy} onClick={quick.dismissServiceFailure}>
              <X className="h-3.5 w-3.5" />
            </Button>
          </div>
          <p className="mt-2 text-error">{failure.error.message}</p>
          {failure.error.hint && <p className="mt-1 text-muted">{failure.error.hint}</p>}
          {quick.serviceTargetChanged && <p className="mt-1 text-muted">{t("versions.serviceChanged")}</p>}
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <Button size="sm" variant="secondary" className="h-auto min-h-8 max-w-full whitespace-normal" disabled={busy || !servicesReady || retryUnavailable}
              onClick={failure.resolve ?? failure.retry}>
              <RotateCw className="h-3.5 w-3.5 shrink-0" />
              {failure.resolve ? t("svc.freePortAndRetry") : `${t("bulk.retry")} · ${t(`common.${failure.action}`)}`}
            </Button>
            <Button size="sm" variant="ghost" className="h-auto min-h-8" disabled={busy} onClick={() => run(() => router.push(`/logs?service=${encodeURIComponent(failure.service.id)}`))}>
              <ScrollText className="h-3.5 w-3.5" />{t("logs.title")}
            </Button>
          </div>
        </div>
      )}
      {readError ? <div role="alert" className="mx-3 mt-2 flex shrink-0 items-start gap-2 rounded-lg bg-error-soft px-3 py-2 text-xs text-error">
        <p className="min-w-0 flex-1 [overflow-wrap:anywhere]">{t("cmd.readFailed")}</p>
        <Button size="sm" variant="ghost" className="h-auto shrink-0 px-2 py-1 text-xs" disabled={refreshing} onClick={retryRead}>{t("packages.reload")}</Button>
      </div> : loading && <p role="status" className="flex shrink-0 items-center gap-2 px-4 py-2 text-xs text-muted"><Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />{t("common.loading")}</p>}
      <CommandList>
        <CommandEmpty>{t("cmd.noResults")}</CommandEmpty>

        <CommandGroup heading={t("cmd.actions")}>
          <CommandItem onSelect={() => run(() => setWizardOpen(true))}>
            <Plus /> {t("cmd.newSite")}
          </CommandItem>
          <CommandItem disabled={busy || !servicesReady || !stacksReady} keywords={["start stack", "LNMP", "启动服务栈"]} onSelect={() => run(startStack)}>
            <Rocket /><span className="min-w-0 [overflow-wrap:anywhere]">{selectedStack ? `${t("dash.startStack")}「${selectedStack.name}」` : t("dash.quickStart")}</span>
          </CommandItem>
          {/* 扫描项目：手上已有一堆项目目录时最快的一条路 */}
          <CommandItem
            value="scan projects folder 扫描项目 导入"
            onSelect={() =>
              run(() => {
                useUI.getState().requestScan();
                router.push("/sites");
              })
            }
          >
            <FolderSearch /> {t("cmd.scanProjects")}
          </CommandItem>
          {/* 体检与诊断：出问题时的第一落点 */}
          <CommandItem
            value="health check 体检 诊断 问题"
            onSelect={() => run(() => router.push("/"))}
          >
            <Activity /> {t("cmd.healthCheck")}
          </CommandItem>
          <CommandItem
            value="diagnostics report 诊断报告 报bug"
            onSelect={() =>
              run(() => {
                useUI.getState().requestTool("diagnostics");
                router.push("/diagnostics");
              })
            }
          >
            <Stethoscope /> {t("cmd.diagnostics")}
          </CommandItem>
          {/* 编辑配置：改 nginx/php 配置的入口 */}
          <CommandItem
            value="config editor nginx php ini 配置"
            onSelect={() =>
              run(() => {

                router.push("/configuration");
              })
            }
          >
            <FileCog /> {t("cmd.configEditor")}
          </CommandItem>
          <CommandItem disabled={busy} onSelect={() => run(stopAll)}>
            <Square /> {t("cmd.stopAll")}
          </CommandItem>
        </CommandGroup>

        <CommandGroup heading={t("cmd.pages")}>
          {PAGES.map((p) => (
            <CommandItem key={p.href} onSelect={() => run(() => router.push(p.href))}>
              <p.icon /> {t(p.labelKey as never)}
            </CommandItem>
          ))}
        </CommandGroup>

        {stacks.length > 0 && (
          <CommandGroup heading={t("cmd.stacks")}>
            {stacks.map((s) => (
              <CommandItem
                key={s.id}
                value={`stack ${s.id} ${s.name}`}
                keywords={[t("cmd.stacks"), t("common.start")]}
                disabled={busy || !servicesReady || !stacksReady}
                onSelect={() => run(() => quick.start(s))}
              >
                <Layers />
                <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">{s.name}</span>
                <span className="shrink-0 text-[10px] text-faint">
                  {s.items.length} {t("cmd.servicesCount")}
                </span>
              </CommandItem>
            ))}
          </CommandGroup>
        )}

        {sites.length > 0 && (
          <CommandGroup heading={t("cmd.sites")}>
            {sites.map((s) => {
              return (
                <CommandItem
                  key={s.id}
                  value={`site ${s.id} ${s.name} ${s.domains.join(" ")}`}
                  keywords={[t("cmd.sites"), t("cmd.openSite")]}
                  disabled={!sitesReady}
                  onSelect={() => run(() => api.openSite(s.id))}
                >
                  <ExternalLink />
                  <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
                    {s.name} <span className="text-faint">· {s.domains[0]}</span>
                  </span>
                  <span className="shrink-0 text-[10px] text-faint">{t("cmd.openSite")}</span>
                </CommandItem>
              );
            })}
          </CommandGroup>
        )}

        {services.length > 0 && (
          <CommandGroup heading={t("cmd.services")}>
            {services.map((s) => {
              const hasProcess = serviceHasProcess(s);
              const transitioning = s.state === "starting" || s.state === "stopping";
              const unknown = s.state === "unknown";
              const needsDependencies = s.missingRequires.length > 0;
              const action = hasProcess ? "stop" : "start";
              const actionLabel = unknown ? t("state.unknown") : transitioning ? t(s.state === "starting" ? "state.starting" : "state.stopping") : t(hasProcess ? "common.stop" : "common.start");
              return (
              <React.Fragment key={s.id}>
                <CommandItem
                  value={`service ${action} ${s.label} ${s.id} ${s.version ?? ""}`}
                  keywords={[t("cmd.services"), actionLabel]}
                  disabled={busy || !servicesReady || transitioning || unknown || (!hasProcess && needsDependencies)}
                  onSelect={() => run(() => quick.serviceAction(s, action))}
                >
                  <StatusLight state={s.state} size={7} />
                  <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
                    {s.label}
                    <span className="block text-[10px] text-faint">{s.id}{s.version ? ` · ${s.version}` : ""} · {t(`state.${s.state}`)}</span>
                    {needsDependencies && <span className="block text-[10px] text-warn">{t("svc.needDeps")}{s.missingRequires.join(", ")}</span>}
                  </span>
                  <span className="shrink-0 text-[10px] text-faint">{actionLabel}</span>
                </CommandItem>
                {/* 重启是日常里比「先停再启」更常用的一步，单独给一条 */}
                {(hasProcess || s.state === "error") && (
                  <CommandItem
                    value={`service restart ${s.label} ${s.id} ${s.version ?? ""} 重启`}
                    keywords={[t("cmd.services"), t("cmd.restart")]}
                    disabled={busy || !servicesReady || transitioning || unknown || needsDependencies}
                    onSelect={() => run(() => quick.serviceAction(s, "restart"))}
                  >
                    <RotateCw />
                    <span className="min-w-0 flex-1 pl-1 [overflow-wrap:anywhere]">
                      {t("cmd.restart")} · {s.label}
                      {s.version && <span className="block text-[10px] text-faint">{s.version}</span>}
                    </span>
                  </CommandItem>
                )}
              </React.Fragment>
              );
            })}
          </CommandGroup>
        )}
      </CommandList>
    </CommandDialog>

    <ConfirmDialog
      open={confirmStopAll}
      onOpenChange={(open) => { if (!busy) setConfirmStopAll(open); }}
      title={t("confirm.stopAll")}
      description={quick.stopDescription}
      confirmText={t(quick.stopReport?.failed.length ? "bulk.retryFailed" : "dash.stopAll")}
      danger
      loading={busy}
      onConfirm={doStopAll}
    >
      {!quick.stopReport && <BulkTargetList targets={quick.stopTargets} />}
      <BulkResult report={quick.stopReport} error={quick.stopError} services={services} targets={quick.stopTargets} busy={quick.busy} />
    </ConfirmDialog>
    </>
  );
}
