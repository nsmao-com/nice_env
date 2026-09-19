"use client";

import * as React from "react";
import { usePathname, useRouter } from "next/navigation";
import { AnimatePresence, motion } from "motion/react";
import { useUI } from "@/lib/store";
import { Sidebar } from "./sidebar";
import { Topbar } from "./topbar";
import { DesktopWindowProvider, useDesktopWindow, WindowControls } from "./titlebar";
import { CommandPalette } from "./command-palette";
import { SiteWizard } from "@/components/sites/site-wizard";
import { Onboarding } from "@/components/sites/onboarding";
import { useInvalidate, useSettings } from "@/lib/hooks";
import { toast } from "sonner";
import { useT } from "@/lib/store";
import { isTauri, listen } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { UpdateDialog } from "@/components/shared/update-dialog";
import * as api from "@/lib/api";

function ShellFrame({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const router = useRouter();
  const wizardOpen = useUI((s) => s.wizardOpen);
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const invalidate = useInvalidate();
  const t = useT();
  const { isDesktop, isMac, maximized } = useDesktopWindow();
  const flushChrome = isDesktop && maximized;

  /* 托盘菜单点「总览 / 套件 / 端口工具…」时后端会发这个事件把窗口带到前台并跳页 */
  React.useEffect(() => {
    let un: (() => void) | undefined;
    listen<{ path: string }>("tray://navigate", (p) => {
      if (p?.path) router.push(p.path);
    }).then((u) => (un = u));
    return () => un?.();
  }, [router]);

  /* 启动服务时自动收掉了端口占用者 → 必须让用户知道（悄悄结束别人的进程不可接受） */
  React.useEffect(() => {
    let un: (() => void) | undefined;
    listen<{ serviceId: string; freed: number[] }>("ports://auto-freed", (p) => {
      if (!p?.freed?.length) return;
      toast.info(t("svc.autoFreedTitle"), {
        description: `${p.freed.map((x) => `:${x}`).join(" ")} — ${t("svc.autoFreedHint")}`,
        duration: 9000,
      });
      invalidate("services");
    }).then((u) => (un = u));
    return () => un?.();
  }, [t, invalidate]);

  /* 导入配置后端口/栈/站点都可能变，统一让相关查询失效 */
  React.useEffect(() => {
    const onImported = () => invalidate("settings", "stacks", "sites", "hosts", "certs", "services", "packages");
    window.addEventListener("nsb:config-imported", onImported);
    return () => window.removeEventListener("nsb:config-imported", onImported);
  }, [invalidate]);

  return (
    <div
      className={cn(
        "nsb-shell relative flex h-screen w-full overflow-hidden",
        isDesktop && "nsb-shell-desktop",
        isMac ? "nsb-shell-mac" : "nsb-shell-win",
        flushChrome && "nsb-shell-maximized"
      )}
    >
      <Sidebar />
      <div className="nsb-workspace relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-card">
        <Topbar />
        <main className="relative min-h-0 flex-1 overflow-y-auto">
          <AnimatePresence mode="wait" initial={false}>
            <motion.div
              key={pathname}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -4 }}
              transition={{ duration: 0.18, ease: "easeOut" }}
              className="mx-auto h-full w-full max-w-[1240px] px-6 py-6"
            >
              {children}
            </motion.div>
          </AnimatePresence>
        </main>
      </div>
      <CommandPalette />
      <SiteWizard
        open={wizardOpen}
        onOpenChange={setWizardOpen}
        onCreated={() => invalidate("sites", "hosts", "certs", "databases")}
      />
      <Onboarding />
      <UpdateWatcher />
      <WindowControls />
    </div>
  );
}

export function AppShell({ children }: { children: React.ReactNode }) {
  return (
    <DesktopWindowProvider>
      <ShellFrame>{children}</ShellFrame>
    </DesktopWindowProvider>
  );
}

/**
 * 启动自动检查更新：桌面端在设置开启时跑一次，
 * 有新版才弹窗（没新版不打扰用户）；自动下载也会在这里触发。
 */
function UpdateWatcher() {
  const [open, setOpen] = React.useState(false);
  const { data: settings } = useSettings();

  React.useEffect(() => {
    if (!isTauri || !settings || !settings.checkUpdateOnLaunch) return;
    // 延迟几秒，避开启动时的并发请求高峰
    const id = setTimeout(async () => {
      try {
        const r = await api.checkUpdates();
        if (r.appUpdate) setOpen(true);
      } catch {
        /* 网络不通就不打扰 */
      }
    }, 4000);
    return () => clearTimeout(id);
  }, [settings]);

  return <UpdateDialog open={open} onOpenChange={setOpen} />;
}

/** 页面标题区（每个页面顶部） */
export function PageHeader({
  title,
  subtitle,
  actions,
}: {
  title: string;
  subtitle?: string;
  actions?: React.ReactNode;
}) {
  return (
    <div className="mb-6 flex items-end justify-between gap-4">
      <div>
        <motion.h1
          initial={{ opacity: 0, y: 6 }}
          animate={{ opacity: 1, y: 0 }}
          className="text-xl font-semibold tracking-tight"
        >
          {title}
        </motion.h1>
        {subtitle && <p className="mt-1 text-[13px] text-muted">{subtitle}</p>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </div>
  );
}
