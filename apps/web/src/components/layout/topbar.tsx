"use client";

import * as React from "react";
import { Search } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { useSystemStats } from "@/lib/hooks";
import { Kbd } from "@/components/ui/misc";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useDesktopWindow, useCaptionDoubleClick } from "./titlebar";

/** 顶部：搜索入口 + 资源微条 + 窗口控制（区域可拖动窗口） */
export function Topbar() {
  const setCommandOpen = useUI((s) => s.setCommandOpen);
  const t = useT();
  const { data: stats } = useSystemStats(3000);
  const { isDesktop } = useDesktopWindow();
  const onCaptionDoubleClick = useCaptionDoubleClick();

  const cpu = stats?.cpuPercent ?? 0;
  const memPct = stats ? Math.round((stats.memUsedMb / Math.max(1, stats.memTotalMb)) * 100) : 0;
  const diskPct = stats
    ? Math.round(((stats.diskTotalGb - stats.diskFreeGb) / Math.max(1, stats.diskTotalGb)) * 100)
    : 0;

  return (
    <header
      className="nsb-topbar flex h-[52px] shrink-0 select-none items-center gap-3 px-4"
      data-tauri-drag-region
      onDoubleClick={onCaptionDoubleClick}
    >
      <button
        type="button"
        onClick={() => setCommandOpen(true)}
        className="nsb-no-drag group relative flex h-8 min-w-0 w-72 items-center gap-2.5 overflow-hidden rounded-full border border-border bg-card-2/40 px-3 text-left text-[13px] text-faint transition-colors hover:border-border-strong hover:bg-card-2/80"
      >
        {/* 悬停时从左向右扫过的强调色微光 */}
        <span
          aria-hidden
          className="pointer-events-none absolute inset-y-0 -left-full w-1/2 bg-gradient-to-r from-transparent via-[hsl(var(--accent-h)_70%_55%/0.13)] to-transparent transition-[left] duration-500 group-hover:left-full"
        />
        <Search className="relative h-3.5 w-3.5 shrink-0" />
        <span className="relative flex-1 truncate">{t("topbar.search")}</span>
        <Kbd className="relative">Ctrl K</Kbd>
      </button>

      <div className="h-px min-w-4 flex-1" data-tauri-drag-region />

      <div className="hidden items-center gap-3 md:flex" data-tauri-drag-region>
        <MicroBar label="CPU" pct={cpu} />
        <MicroBar label="RAM" pct={memPct} />
        <MicroBar label="DISK" pct={diskPct} />
      </div>

      {isDesktop && (
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="flex items-center gap-1.5 text-[11px] text-faint" data-tauri-drag-region>
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
        <div className="flex items-center gap-1.5" data-tauri-drag-region>
          <span className="text-[10px] font-medium tracking-wide text-faint">{label}</span>
          <div className="h-1 w-14 overflow-hidden rounded-full bg-card-2" data-tauri-drag-region>
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
