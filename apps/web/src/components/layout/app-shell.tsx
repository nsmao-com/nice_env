"use client";

import * as React from "react";
import Link from "next/link";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, RefreshCw } from "lucide-react";
import { useRouter } from "next/navigation";
import { useUI } from "@/lib/store";
import { Sidebar } from "./sidebar";
import { Topbar } from "./topbar";
import { DesktopWindowProvider, useDesktopWindow, WindowControls } from "./titlebar";
import { CommandPalette } from "./command-palette";
import { SiteWizard } from "@/components/sites/site-wizard";
import { Onboarding } from "@/components/sites/onboarding";
import { toastError, useInvalidate, useSettings } from "@/lib/hooks";
import { toast } from "sonner";
import { useT } from "@/lib/store";
import { isTauri, listen } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { UpdateDialog } from "@/components/shared/update-dialog";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";

function ShellFrame({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const wizardOpen = useUI((s) => s.wizardOpen);
  const wizardKind = useUI((s) => s.wizardKind);
  const wizardProject = useUI((s) => s.wizardProject);
  const wizardProjectDefaults = useUI((s) => s.wizardProjectDefaults);
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
            <RecoveryAlert />
            {children}
          </div>
        </main>
      </div>
      <CommandPalette />
      <CertAlertListener />
      <SiteWizard
        open={wizardOpen}
        initialKind={wizardKind}
        existingProject={wizardProject}
        existingDefaults={wizardProjectDefaults}
        onOpenChange={setWizardOpen}
        onCreated={() => invalidate("sites", "hosts", "certs", "databases")}
      />
      <Onboarding />
      <UpdateWatcher />
      <WindowControls />
    </div>
  );
}

function RecoveryAlert() {
  const t = useT();
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["process-recovery"], queryFn: api.processRecoveryStatus, staleTime: Infinity, retry: false });
  const busyRef = React.useRef(false);
  const [busy, setBusy] = React.useState(false);
  const retry = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      if (query.isError) {
        await query.refetch();
      } else {
        const report = await api.recoverProcesses();
        client.setQueryData(["process-recovery"], report);
        await Promise.all(["services", "sites", "watchdog"].map((key) => client.invalidateQueries({ queryKey: [key] })));
        if (report.unresolved.length === 0) toast.success(t("recovery.done"));
      }
    } catch (error) { toastError(error); }
    finally { busyRef.current = false; setBusy(false); }
  };
  if (!query.isError && !query.data?.unresolved.length) return null;
  return (
    <section role="alert" className="mb-5 min-w-0 space-y-3 rounded-xl border border-warn/25 bg-warn-soft px-4 py-3">
      <div className="flex items-start gap-2 text-warn">
        <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
        <h2 className="min-w-0 text-[13px] font-medium">{t(query.isError ? "recovery.loadFailed" : "recovery.title")}</h2>
      </div>
      <p className="text-xs leading-relaxed text-secondary">{t("recovery.hint")}</p>
      {!query.isError && <ul className="max-h-36 space-y-1 overflow-y-auto break-words text-xs leading-relaxed text-secondary">
        {query.data?.unresolved.map((message, index) => <li key={`${index}:${message}`}>{message}</li>)}
      </ul>}
      <div className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant="outline" disabled={busy || (!isTauri && !query.isError)} onClick={() => void retry()}>
          <RefreshCw className={cn("h-3.5 w-3.5", busy && "animate-spin")} />
          {t(busy ? "recovery.checking" : query.isError ? "recovery.reload" : "recovery.retry")}
        </Button>
        <Button size="sm" variant="ghost" asChild><Link href="/tools">{t("recovery.tools")}</Link></Button>
      </div>
      {!isTauri && <p className="text-[11px] text-faint">{t("recovery.preview")}</p>}
    </section>
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
 * 后端按状态跃迁去重；恢复后再次故障仍需展示。
 */
function CertAlertListener() {
  const t = useT();
  React.useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    listen<{ host: string; state: string; message: string }>("certmonitor://alert", (p) => {
      if (disposed || !p?.host) return;
      const expired = p.state === "expired";
      const error = p.state === "error";
      (expired ? toast.error : error ? toast.error : toast.warning)(
        `${p.host} · ${t(expired ? "monitor.expired" : error ? "monitor.checkFailed" : "monitor.expiring")}`,
        { description: p.message, duration: 10000 }
      );
    }).then((u) => { if (disposed) u(); else un = u; });
    return () => { disposed = true; un?.(); };
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
