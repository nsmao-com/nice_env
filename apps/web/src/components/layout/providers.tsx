"use client";

import * as React from "react";
import { ThemeProvider, useTheme } from "next-themes";
import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Toaster } from "@/components/ui/sonner";
import { isTauri } from "@/lib/backend";
import * as api from "@/lib/api";
import { applyAppearance } from "@/lib/appearance";

/**
 * 外观同步：桌面端以 SQLite 设置为唯一事实源（覆盖 webview localStorage 里的旧值）；
 * 主题色 / 字体 / 字号 / 滚动条也都由这里统一写到 :root，避免各页面各写一份。
 * 必须挂在 QueryClientProvider 内部（用了 useQuery）。
 */
function AppearanceSync() {
  const { setTheme } = useTheme();
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

  return null;
}

export function Providers({ children }: { children: React.ReactNode }) {
  const [client] = React.useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 1500,
            retry: 1,
            refetchOnWindowFocus: false,
          },
        },
      })
  );

  return (
    <ThemeProvider attribute="class" defaultTheme="light" enableSystem>
      <QueryClientProvider client={client}>
        <AppearanceSync />
        <TooltipProvider delayDuration={300}>
          {children}
          <Toaster />
        </TooltipProvider>
      </QueryClientProvider>
    </ThemeProvider>
  );
}
