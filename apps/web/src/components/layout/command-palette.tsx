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
} from "lucide-react";
import { CommandDialog, CommandGroup, CommandInput, CommandItem, CommandList, CommandEmpty } from "@/components/ui/command";
import { useUI, useT } from "@/lib/store";
import { useServices, useSites, useStacks, toastError, toastPortConflict, usePorts, siteUrl } from "@/lib/hooks";
import * as api from "@/lib/api";
import { StatusLight } from "@/components/shared/status-light";
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
  const [busy, setBusy] = React.useState(false);

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

  const startStack = async () => {
    // 有保存的服务栈就直接用它（第一个 = 最常用的那个）
    const chosen = stacks[0];
    if (chosen) {
      try {
        const report = await api.startStack(chosen.id);
        if (report.failed.length > 0) {
          toast.warning(t("dashboard.stackPartial"), {
            description: report.failed.map((f) => `${f.serviceId}: ${f.error.message}`).join("\n"),
            duration: 9000,
          });
        } else {
          toast.success(t("dashboard.stackStarted"), { description: chosen.name });
        }
      } catch (e) {
        if (!toastPortConflict(e, { onResolved: () => void startStack() })) toastError(e);
      }
      return;
    }
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
            /* 未安装的跳过 */
          }
        }
      })(),
      { loading: t("dashboard.startingStack"), success: t("dashboard.stackStarted"), error: t("cmd.startFail") }
    );
  };

  const stopAll = async () => {
    setOpen(false);
    setConfirmStopAll(true);
  };

  /** 确认后真正执行 */
  const doStopAll = async () => {
    setBusy(true);
    try {
      for (const s of services) {
        if (s.state === "running" || s.state === "starting") {
          await api.stopService(s.id).catch(() => undefined);
        }
      }
      toast.success(t("dashboard.allStopped"));
      setConfirmStopAll(false);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
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
          <CommandItem onSelect={() => run(startStack)}>
            <Rocket /> {t("cmd.quickStart")}
          </CommandItem>
          <CommandItem onSelect={() => run(stopAll)}>
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
                onSelect={() =>
                  run(async () => {
                    const report = await api.startStack(s.id);
                    if (report.failed.length > 0) {
                      toast.warning(t("dashboard.stackPartial"), {
                        description: report.failed.map((f) => `${f.serviceId}: ${f.error.message}`).join("\n"),
                        duration: 9000,
                      });
                    } else {
                      toast.success(t("dashboard.stackStarted"), { description: s.name });
                    }
                  })
                }
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
              const url = siteUrl(s, ports.http, ports.https);
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
              <CommandItem
                key={s.id}
                value={`service ${s.label} ${s.id}`}
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
            ))}
          </CommandGroup>
        )}
      </CommandList>
    </CommandDialog>

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
      loading={busy}
      onConfirm={doStopAll}
    />
    </>
  );
}
