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
              // Apple 分组标题：小号次级文字，不做全大写
              <span className="mb-1 px-2.5 text-[11px] font-medium text-faint">
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
                    "group relative flex h-9 items-center gap-2.5 rounded-full px-2.5 text-[13px] font-medium transition-colors active:scale-[0.98]",
                    active
                      ? "text-foreground"
                      : "text-secondary hover:bg-fill hover:text-foreground",
                    collapsed && "justify-center px-0"
                  )}
                >
                  {active && (
                    /* 悬浮选中胶囊：白色（深色抬升）+ 极淡投影 —— Apple 侧栏选中态，
                       不再用强调色指示条与光晕 */
                    <motion.span
                      layoutId="nav-active"
                      className="absolute inset-0 rounded-full bg-card shadow-[var(--thumb-shadow)]"
                      transition={{ type: "spring", stiffness: 500, damping: 38 }}
                    />
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
                "flex h-9 flex-1 items-center gap-2.5 rounded-full px-2.5 text-[13px] font-medium transition-colors",
                pathname === "/settings"
                  ? "bg-card text-foreground shadow-[var(--thumb-shadow)]"
                  : "text-secondary hover:bg-fill hover:text-foreground",
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
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-faint transition-colors hover:bg-fill hover:text-secondary"
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
