"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
import {
  Globe,
  ExternalLink,
  FolderOpen,
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
  FileText,
} from "lucide-react";
import { CommandDialog, CommandGroup, CommandInput, CommandItem, CommandList, CommandEmpty } from "@/components/ui/command";
import { useUI, useT } from "@/lib/store";
import { useServices, useSites, useStacks, toastError, useQuickServiceActions, usePorts, siteUrl } from "@/lib/hooks";
import * as api from "@/lib/api";
import { StatusLight } from "@/components/shared/status-light";
import { BulkResult } from "@/components/shared/bulk-actions";
import { ConfirmDialog } from "@/components/shared/misc";

const PAGES: { href: string; icon: typeof LayoutDashboard; labelKey: string }[] = [
  { href: "/", icon: LayoutDashboard, labelKey: "cmd.page.dashboard" },
  { href: "/sites", icon: Globe, labelKey: "cmd.page.sites" },
  { href: "/packages", icon: Boxes, labelKey: "cmd.page.packages" },
  { href: "/stacks", icon: Layers, labelKey: "cmd.page.stacks" },
  { href: "/databases", icon: Database, labelKey: "cmd.page.databases" },
  { href: "/tls", icon: ShieldCheck, labelKey: "cmd.page.tls" },
  { href: "/proxy", icon: Waypoints, labelKey: "cmd.page.proxy" },
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
  const { data: services } = useServices(0);
  const { data: sites } = useSites();
  const { data: stacks } = useStacks();
  const ports = usePorts();
  const [confirmStopAll, setConfirmStopAll] = React.useState(false);
  const quick = useQuickServiceActions(services, stacks);
  const busy = quick.busy;

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

  const startStack = () => quick.start();
  const stopAll = () => {
    quick.prepareStop();
    setOpen(false);
    setConfirmStopAll(true);
  };
  const doStopAll = async () => {
    const report = await quick.stop(quick.stopReport?.failed.map((f) => f.serviceId));
    if (report && !report.failed.length) setConfirmStopAll(false);
  };

  return (
    <>
    <CommandDialog open={open} onOpenChange={setOpen}>
      <CommandInput placeholder={t("cmd.placeholder")} />
      <CommandList>
        <CommandEmpty>{t("cmd.noResults")}</CommandEmpty>

        <CommandGroup heading={t("cmd.actions")}>
          <CommandItem onSelect={() => run(() => setWizardOpen(true))}>
            <Plus /> {t("cmd.newSite")}
          </CommandItem>
          <CommandItem disabled={busy} onSelect={() => run(startStack)}>
            <Rocket /> {t("cmd.quickStart")}
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
                router.push("/tools");
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
                useUI.getState().requestTool("config");
                router.push("/tools");
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
            {stacks.slice(0, 8).map((s) => (
              <CommandItem
                key={s.id}
                value={`stack ${s.name}`}
                disabled={busy}
                onSelect={() => run(() => quick.start(s))}
              >
                <Layers />
                <span className="flex-1 truncate">{s.name}</span>
                <span className="text-[10px] text-faint">
                  {s.items.length} {t("cmd.servicesCount")}
                </span>
              </CommandItem>
            ))}
          </CommandGroup>
        )}

        {sites.length > 0 && (
          <CommandGroup heading={t("cmd.sites")}>
            {sites.slice(0, 8).map((s) => {
              const url = siteUrl(s, ports);
              return (
                <CommandItem
                  key={s.id}
                  value={`site ${s.name} ${s.domains.join(" ")}`}
                  onSelect={() => run(() => api.openInBrowser(url))}
                >
                  <ExternalLink />
                  <span className="flex-1 truncate">
                    {s.name} <span className="text-faint">· {s.domains[0]}</span>
                  </span>
                  <span className="text-[10px] text-faint">{t("cmd.openSite")}</span>
                </CommandItem>
              );
            })}
          </CommandGroup>
        )}

        {services.length > 0 && (
          <CommandGroup heading={t("cmd.services")}>
            {services.map((s) => (
              <React.Fragment key={s.id}>
                <CommandItem
                  value={`service start stop ${s.label} ${s.id}`}
                  onSelect={() =>
                    run(() =>
                      s.state === "running" ? api.stopService(s.id) : api.startService(s.id)
                    )
                  }
                >
                  <StatusLight state={s.state} size={7} />
                  <span className="flex-1">{s.label}</span>
                  <span className="text-[10px] text-faint">
                    {s.state === "running" ? t("common.stop") : t("common.start")}
                  </span>
                </CommandItem>
                {/* 重启是日常里比「先停再启」更常用的一步，单独给一条 */}
                {s.state === "running" && (
                  <CommandItem
                    value={`service restart ${s.label} ${s.id} 重启`}
                    onSelect={() =>
                      run(async () => {
                        await api.restartService(s.id);
                        toast.success(`${s.label} · ${t("common.running")}`);
                      })
                    }
                  >
                    <RotateCw />
                    <span className="flex-1 pl-1">
                      {t("cmd.restart")} · {s.label}
                    </span>
                  </CommandItem>
                )}
              </React.Fragment>
            ))}
          </CommandGroup>
        )}
      </CommandList>
    </CommandDialog>

    <ConfirmDialog
      open={confirmStopAll}
      onOpenChange={(open) => { if (!busy) setConfirmStopAll(open); }}
      title={t("confirm.stopAll")}
      description={t(quick.stopReport?.failed.length ? "bulk.retryStopHint" : "confirm.stopAllDesc").replace(
        "{count}",
        String(quick.stopTargetCount)
      )}
      confirmText={t(quick.stopReport?.failed.length ? "bulk.retryFailed" : "dash.stopAll")}
      danger
      loading={busy}
      onConfirm={doStopAll}
    >
      <BulkResult report={quick.stopReport} error={quick.stopError} services={services} busy={quick.busy} />
    </ConfirmDialog>
    </>
  );
}
