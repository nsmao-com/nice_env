"use client";

import * as React from "react";
import { Search } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { useSystemStats } from "@/lib/hooks";
import { Kbd } from "@/components/ui/misc";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useDesktopWindow } from "./titlebar";

/** 顶部：搜索入口 + 资源微条 + 窗口控制（区域可拖动窗口） */
export function Topbar() {
  const setCommandOpen = useUI((s) => s.setCommandOpen);
  const t = useT();
  const { data: stats } = useSystemStats(3000);
  const { isDesktop } = useDesktopWindow();

  const cpu = stats?.cpuPercent ?? 0;
  const memPct = stats ? Math.round((stats.memUsedMb / Math.max(1, stats.memTotalMb)) * 100) : 0;
  const diskPct = stats
    ? Math.round(((stats.diskTotalGb - stats.diskFreeGb) / Math.max(1, stats.diskTotalGb)) * 100)
    : 0;

  return (
    // data-tauri-drag-region="deep"：整条顶栏都是拖拽区（点子元素也能拖）。
    // 裸属性只在本元素被点中时生效，文字/图标上会拖不动；
    // 双击最大化由 Tauri 原生脚本处理，不要再挂 React 的 onDoubleClick（会切换两次互相抵消）。
    <header
      className="nsb-topbar flex h-[52px] shrink-0 select-none items-center gap-2 px-3 sm:gap-3 sm:px-4"
      data-tauri-drag-region="deep"
    >
      <button
        type="button"
        onClick={() => setCommandOpen(true)}
        // 搜索框 = Liquid Glass 胶囊（功能层），悬停只略微提高不透明度
        className="glass nsb-no-drag group relative flex h-8 min-w-0 w-full max-w-72 flex-1 items-center gap-2.5 overflow-hidden rounded-full px-3 text-left text-[13px] text-faint transition-[background-color] hover:bg-[color-mix(in_srgb,var(--glass-tint)_calc(var(--glass-alpha)+12%),transparent)] sm:flex-none sm:w-72"
      >
        <Search className="relative h-3.5 w-3.5 shrink-0" />
        <span className="relative flex-1 truncate">{t("topbar.search")}</span>
        <Kbd className="relative">Ctrl K</Kbd>
      </button>

      <div className="h-px min-w-4 flex-1" />

      <div className="hidden items-center gap-3 md:flex">
        <MicroBar label="CPU" pct={cpu} />
        <MicroBar label="RAM" pct={memPct} />
        <MicroBar label="DISK" pct={diskPct} />
      </div>

      {isDesktop && (
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="flex items-center gap-1.5 text-[11px] text-faint">
              <span className="relative flex h-1.5 w-1.5">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-running opacity-60" />
                <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-running" />
              </span>
              {t("topbar.tray")}
            </span>
          </TooltipTrigger>
          <TooltipContent>{t("topbar.trayHint")}</TooltipContent>
        </Tooltip>
      )}
    </header>
  );
}

function MicroBar({ label, pct }: { label: string; pct: number }) {
  const clamped = Math.min(100, Math.max(0, pct));
  const color = clamped > 85 ? "bg-error" : clamped > 60 ? "bg-warn" : "bg-running";
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div className="flex items-center gap-1.5">
          <span className="text-[10px] font-medium tracking-wide text-faint">{label}</span>
          <div className="h-1 w-14 overflow-hidden rounded-full bg-card-2">
            <div
              className={`h-full rounded-full ${color} transition-all duration-500`}
              style={{ width: `${clamped}%` }}
            />
          </div>
          <span className="w-7 text-right text-[10px] tabular text-faint">{Math.round(clamped)}%</span>
        </div>
      </TooltipTrigger>
      <TooltipContent>
        {label} {clamped.toFixed(0)}%
      </TooltipContent>
    </Tooltip>
  );
}
