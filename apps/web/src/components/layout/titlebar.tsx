"use client";

import * as React from "react";
import { Minus, Square, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

type DesktopWindow = {
  mounted: boolean;
  isMac: boolean;
  isDesktop: boolean;
  maximized: boolean;
  minimize: () => void;
  toggleMaximize: () => void;
  close: () => void;
};

const DesktopWindowContext = React.createContext<DesktopWindow>({
  mounted: false,
  isMac: false,
  isDesktop: false,
  maximized: false,
  minimize: () => undefined,
  toggleMaximize: () => undefined,
  close: () => undefined,
});

export function useDesktopWindow() {
  return React.useContext(DesktopWindowContext);
}

// 双击拖拽区最大化 / 还原由 Tauri 原生脚本（internal_toggle_maximize）处理：
// macOS 上 mouseup 触发、可拖动取消，行为与系统一致。应用层不要再挂
// onDoubleClick 切换，否则会和原生脚本各切换一次，看起来像没反应。

export function DesktopWindowProvider({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = React.useState(false);
  const [isMac, setIsMac] = React.useState(false);
  const [maximized, setMaximized] = React.useState(false);

  React.useEffect(() => {
    setMounted(true);
    setIsMac(/Mac/i.test(navigator.userAgent));
    if (!isTauri) return;
    const w = getCurrentWindow();
    let un: (() => void) | undefined;
    w.isMaximized()
      .then(setMaximized)
      .catch(() => undefined);
    w.onResized(async () => {
      try {
        setMaximized(await w.isMaximized());
      } catch {
        /* ignore */
      }
    }).then((u) => {
      un = u;
    });
    return () => un?.();
  }, []);

  const api = React.useMemo<DesktopWindow>(() => {
    const run = (fn: (w: ReturnType<typeof getCurrentWindow>) => void) => {
      if (!isTauri) return;
      try {
        fn(getCurrentWindow());
      } catch {
        /* ignore */
      }
    };
    return {
      mounted,
      isMac,
      isDesktop: mounted && isTauri,
      maximized,
      minimize: () => run((w) => void w.minimize()),
      toggleMaximize: () => run((w) => void w.toggleMaximize()),
      close: () => run((w) => void w.close()),
    };
  }, [mounted, isMac, maximized]);

  return <DesktopWindowContext.Provider value={api}>{children}</DesktopWindowContext.Provider>;
}

function RestoreIcon() {
  return (
    <svg viewBox="0 0 12 12" className="h-[11px] w-[11px]" fill="none" aria-hidden>
      <rect x="1.25" y="3.5" width="7.25" height="7.25" rx="1.1" stroke="currentColor" strokeWidth="1.15" />
      <path
        d="M3.6 3.5V2.4A1.15 1.15 0 0 1 4.75 1.25h5.85A1.15 1.15 0 0 1 11.75 2.4v5.85A1.15 1.15 0 0 1 10.6 9.4H9.4"
        stroke="currentColor"
        strokeWidth="1.15"
      />
    </svg>
  );
}

/** Windows / Linux 窗口按钮，贴在窗口最右上角的灰底上，不进入内容区。 */
export function WindowControls({ className }: { className?: string }) {
  const t = useT();
  const { mounted, isMac, maximized, minimize, toggleMaximize, close } = useDesktopWindow();
  if (!mounted || isMac) return null;

  return (
    <div className={cn("nsb-window-controls nsb-no-drag flex items-stretch", className)}>
      <CtrlBtn label={t("titlebar.min")} onClick={minimize}>
        <Minus className="h-3.5 w-3.5" strokeWidth={1.7} />
      </CtrlBtn>
      <CtrlBtn label={maximized ? t("titlebar.restore") : t("titlebar.max")} onClick={toggleMaximize}>
        {maximized ? <RestoreIcon /> : <Square className="h-[11px] w-[11px]" strokeWidth={1.7} />}
      </CtrlBtn>
      <CtrlBtn label={t("titlebar.close")} danger onClick={close}>
        <X className="h-3.5 w-3.5" strokeWidth={1.7} />
      </CtrlBtn>
    </div>
  );
}

function CtrlBtn({
  label,
  danger,
  onClick,
  children,
}: {
  label: string;
  danger?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={label}
          onClick={(e) => {
            e.stopPropagation();
            onClick();
          }}
          onDoubleClick={(e) => e.stopPropagation()}
          className={cn(
            "nsb-no-drag flex h-full w-[46px] items-center justify-center text-secondary transition-colors duration-100",
            danger
              ? "hover:bg-[#e81123] hover:text-white"
              : "hover:bg-black/8 hover:text-foreground dark:hover:bg-white/10"
          )}
        >
          {children}
        </button>
      </TooltipTrigger>
      <TooltipContent side="bottom">{label}</TooltipContent>
    </Tooltip>
  );
}
