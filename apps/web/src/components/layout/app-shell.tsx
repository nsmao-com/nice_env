"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
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
          {/* 路由内容由 Next 管理；避免退出动画保留旧树或让返回页面停留在透明状态。 */}
          <div className="mx-auto h-full w-full max-w-[1240px] px-3 py-4 sm:px-6 sm:py-6">
            {children}
          </div>
        </main>
      </div>
      <CommandPalette />
      <CertAlertListener />
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
 * 证书监控告警：后端 certmonitor://alert 事件 → 全局 toast。
 * 只在状态跃迁时发（后端去重），这里按 host+state 再兜个底防重复。
 */
function CertAlertListener() {
  const t = useT();
  const seen = React.useRef(new Set<string>());
  React.useEffect(() => {
    let un: (() => void) | undefined;
    listen<{ host: string; state: string; message: string }>("certmonitor://alert", (p) => {
      if (!p?.host) return;
      const key = `${p.host}|${p.state}`;
      if (seen.current.has(key)) return;
      seen.current.add(key);
      const expired = p.state === "expired";
      const error = p.state === "error";
      (expired ? toast.error : error ? toast.error : toast.warning)(
        `${p.host} · ${t(expired ? "monitor.expired" : error ? "monitor.checkFailed" : "monitor.expiring")}`,
        { description: p.message, duration: 10000 }
      );
    }).then((u) => (un = u));
    return () => un?.();
  }, [t]);
  return null;
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
    <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
      <div className="min-w-0 flex-1 basis-[180px]">
        <h1 className="flex items-center gap-2 text-[19px] font-semibold tracking-[-0.02em]">
          {/* 强调色小竖条：给标题一个视觉锚点，也随主题色变化（无光晕，Apple 克制） */}
          <span
            aria-hidden
            className="h-[18px] w-[3px] shrink-0 rounded-full bg-primary"
          />
          <span className="truncate">{title}</span>
        </h1>
        {subtitle && <p className="mt-1.5 pl-[11px] text-[13px] text-muted">{subtitle}</p>}
      </div>
      {actions && <div className="flex max-w-full flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}
