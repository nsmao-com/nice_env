"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { motion } from "motion/react";
import {
  LayoutDashboard,
  Globe,
  Boxes,
  Layers,
  Database,
  ShieldCheck,
  Waypoints,
  Wrench,
  ScrollText,
  Settings,
  PanelLeftClose,
  PanelLeftOpen,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useUI, useT } from "@/lib/store";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useCaptionDoubleClick, useDesktopWindow } from "./titlebar";
import { AppMenu } from "./app-menu";

const NAV = [
  {
    group: "dev",
    items: [
      { href: "/", icon: LayoutDashboard, key: "nav.dashboard" as const },
      { href: "/sites", icon: Globe, key: "nav.sites" as const },
      { href: "/packages", icon: Boxes, key: "nav.packages" as const },
      { href: "/stacks", icon: Layers, key: "nav.stacks" as const },
    ],
  },
  {
    group: "ops",
    items: [
      { href: "/databases", icon: Database, key: "nav.databases" as const },
      { href: "/tls", icon: ShieldCheck, key: "nav.tls" as const },
      { href: "/proxy", icon: Waypoints, key: "nav.proxy" as const },
      { href: "/tools", icon: Wrench, key: "nav.tools" as const },
      { href: "/logs", icon: ScrollText, key: "nav.logs" as const },
    ],
  },
];

export function Sidebar() {
  const collapsed = useUI((s) => s.sidebarCollapsed);
  const toggle = useUI((s) => s.toggleSidebar);
  const t = useT();
  const pathname = usePathname();
  const { isMac, isDesktop } = useDesktopWindow();
  const onCaptionDoubleClick = useCaptionDoubleClick();

  return (
    <motion.aside
      animate={{ width: collapsed ? 68 : 232 }}
      transition={{ type: "spring", stiffness: 420, damping: 36 }}
      className="nsb-sidebar relative z-20 flex h-full shrink-0 select-none flex-col"
    >
      <div
        className={cn(
          "nsb-sidebar-brand flex shrink-0 items-center gap-2.5 px-3.5",
          isDesktop && isMac && !collapsed && "pl-[78px]",
          isDesktop && isMac && collapsed && "px-0"
        )}
        onDoubleClick={onCaptionDoubleClick}
      >
        {!(isDesktop && isMac && collapsed) && <AppMenu collapsed={collapsed} />}
        <div className="h-full min-w-2 flex-1 self-stretch" data-tauri-drag-region />
      </div>

      <nav className="nsb-no-drag flex flex-1 flex-col gap-4 overflow-y-auto overflow-x-hidden px-2.5 py-1.5 no-scrollbar">
        {NAV.map((section) => (
          <div key={section.group} className="flex flex-col gap-0.5">
            {!collapsed && (
              <span className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/65">
                {t(`nav.group.${section.group}` as "nav.group.dev")}
              </span>
            )}
            {section.items.map((item) => {
              const active = pathname === item.href;
              const label = t(item.key);
              const body = (
                <Link
                  href={item.href}
                  className={cn(
                    "group relative flex h-9 items-center gap-2.5 rounded-lg px-2.5 text-[13px] font-medium transition-colors",
                    active
                      ? "text-foreground"
                      : "text-muted hover:bg-card-2/60 hover:text-foreground",
                    collapsed && "justify-center px-0"
                  )}
                >
                  {active && (
                    <>
                      {/* 悬浮底：实色 + 1px 高光，边界比纯描边更干净 */}
                      <motion.span
                        layoutId="nav-active"
                        className="card-fill absolute inset-0 rounded-lg shadow-[var(--shadow-card)] ring-1 ring-inset ring-[var(--thumb-ring)]"
                        transition={{ type: "spring", stiffness: 500, damping: 38 }}
                      />
                      {/* 左侧强调色指示条：一眼看出当前页，且随主题色变化 */}
                      <motion.span
                        layoutId="nav-active-bar"
                        className="absolute left-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-full bg-primary shadow-[0_0_8px_0_var(--accent-glow)]"
                        transition={{ type: "spring", stiffness: 500, damping: 38 }}
                      />
                    </>
                  )}
                  <item.icon
                    className={cn(
                      "relative h-[17px] w-[17px] shrink-0 transition-colors",
                      active ? "text-primary" : "text-faint group-hover:text-secondary"
                    )}
                    strokeWidth={active ? 2.15 : 1.8}
                  />
                  {!collapsed && <span className="relative truncate">{label}</span>}
                </Link>
              );
              return collapsed ? (
                <Tooltip key={item.href}>
                  <TooltipTrigger asChild>{body}</TooltipTrigger>
                  <TooltipContent side="right">{label}</TooltipContent>
                </Tooltip>
              ) : (
                <React.Fragment key={item.href}>{body}</React.Fragment>
              );
            })}
          </div>
        ))}
      </nav>

      <div className="nsb-no-drag flex items-center gap-0.5 p-2.5">
        <Tooltip>
          <TooltipTrigger asChild>
            <Link
              href="/settings"
              className={cn(
                "flex h-9 flex-1 items-center gap-2.5 rounded-lg px-2.5 text-[13px] font-medium transition-colors",
                pathname === "/settings"
                  ? "card-fill text-foreground shadow-[var(--shadow-card)] ring-1 ring-inset ring-[var(--thumb-ring)]"
                  : "text-muted hover:bg-card-2/60 hover:text-foreground",
                collapsed && "justify-center px-0"
              )}
            >
              <Settings className="h-[17px] w-[17px] shrink-0" strokeWidth={1.8} />
              {!collapsed && <span>{t("nav.settings")}</span>}
            </Link>
          </TooltipTrigger>
          {collapsed && <TooltipContent side="right">{t("nav.settings")}</TooltipContent>}
        </Tooltip>
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={toggle}
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-faint transition-colors hover:bg-card-2/70 hover:text-secondary"
            >
              {collapsed ? <PanelLeftOpen className="h-4 w-4" /> : <PanelLeftClose className="h-4 w-4" />}
            </button>
          </TooltipTrigger>
          <TooltipContent side="right">{t("palette.collapse")}</TooltipContent>
        </Tooltip>
      </div>
    </motion.aside>
  );
}
