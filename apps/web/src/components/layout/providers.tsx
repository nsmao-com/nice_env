"use client";

import * as React from "react";
import { MotionConfig } from "motion/react";
import { ThemeProvider, useTheme } from "next-themes";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import type { DownloadProgress } from "@nsb/schema";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Toaster } from "@/components/ui/sonner";
import { isTauri, listen } from "@/lib/backend";
import * as api from "@/lib/api";
import { applyAppearance } from "@/lib/appearance";
import { toastError } from "@/lib/hooks";
import { useInstallTasks } from "@/lib/install-tasks";

/**
 * 外观同步：桌面端以 SQLite 设置为唯一事实源（覆盖 webview localStorage 里的旧值）；
 * 主题色 / 字体 / 字号 / 滚动条也都由这里统一写到 :root，避免各页面各写一份。
 * 必须挂在 QueryClientProvider 内部（用了 useQuery）。
 */
function AppearanceSync() {
  const { setTheme } = useTheme();
  const qc = useQueryClient();
  const readySent = React.useRef(false);
  const { data } = useQuery({
    queryKey: ["settings"],
    queryFn: api.getSettings,
    staleTime: 30_000,
    enabled: isTauri,
    retry: 0,
  });

  React.useEffect(() => {
    if (!isTauri) return;
    api
      .getSettings()
      .then((s) => {
        // 「跟随系统」必须原样交给 next-themes：这里统一按亮度分辨率会把系统主题丢掉
        if (s.appearance === "system") setTheme("system");
        else setTheme(s.appearance === "dark" ? "dark" : "light");
      })
      .catch(() => undefined);
  }, [setTheme]);

  React.useEffect(() => {
    if (data) applyAppearance(data);
  }, [data]);

  React.useEffect(() => {
    if (!isTauri || !data || readySent.current) return;
    readySent.current = true;
    // 设置读取和 React 挂载都完成后才确认页面就绪；交接完成后重取准备期间受限的数据。
    void api.frontendReady().then((activated) => {
      if (activated) void qc.invalidateQueries();
    }).catch(toastError);
  }, [data, qc]);

  return null;
}

/**
 * 安装任务桥：下载进度事件全局只订阅一次写进 store；任一安装完成时刷新相关数据。
 * 安装因此与弹窗、页面彻底解耦——关掉弹窗、切到别的菜单，都不影响它在后台装完。
 */
function InstallTasksBridge() {
  const qc = useQueryClient();

  React.useEffect(() => {
    let alive = true;
    let un: (() => void) | undefined;
    listen<DownloadProgress>("download://progress", (p) => useInstallTasks.getState().setProgress(p))
      .then((u) => {
        if (alive) un = u;
        else u();
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      un?.();
    };
  }, []);

  React.useEffect(
    () =>
      useInstallTasks.subscribe((s, prev) => {
        const justDone = Object.values(s.tasks).some(
          (task) => task.status === "done" && prev.tasks[task.key]?.status !== "done"
        );
        if (!justDone) return;
        for (const key of ["packages", "services", "version-catalogs", "pathenv"]) {
          qc.invalidateQueries({ queryKey: [key] });
        }
      }),
    [qc]
  );

  return null;
}

function MotionPreferences({ children }: { children: React.ReactNode }) {
  const { data } = useQuery({ queryKey: ["settings"], queryFn: api.getSettings, staleTime: 30000 });
  return <MotionConfig reducedMotion={data?.reduceMotion ? "always" : "user"}>{children}</MotionConfig>;
}

export function Providers({ children }: { children: React.ReactNode }) {
  const [client] = React.useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            // 请求经本地 IPC（浏览器预览走内存后端）；断网不能暂停设置、配置或数据库读取。
            networkMode: "always",
            staleTime: 1500,
            retry: 1,
            refetchOnWindowFocus: false,
            refetchOnReconnect: true,
          },
          // PATH 等写入必须在用户操作时执行并返回结果，不能排队等联网后再执行。
          mutations: { networkMode: "always", retry: false },
        },
      })
  );

  return (
    <ThemeProvider attribute="class" defaultTheme="light" enableSystem>
      <QueryClientProvider client={client}>
        <AppearanceSync />
        <InstallTasksBridge />
        <MotionPreferences><TooltipProvider delayDuration={300}>
          {children}
          <Toaster />
        </TooltipProvider></MotionPreferences>
      </QueryClientProvider>
    </ThemeProvider>
  );
}
