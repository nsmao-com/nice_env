"use client";

import * as React from "react";
import { motion, AnimatePresence } from "motion/react";
import { Check, ChevronDown, Download, Loader2, Power, RefreshCw, Trash2, WifiOff } from "lucide-react";
import type { PackageView, RemoteVersion } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

/** 下拉里的一项：清单内置版本与远程版本合并后的统一视图 */
export interface VersionItem {
  version: string;
  installed: boolean;
  /** 单实例服务的「使用中版本」 */
  active: boolean;
  running: boolean;
  /** 来自远程枚举（清单里没有） */
  remote?: RemoteVersion;
  sizeBytes?: number;
  note?: string;
  prerelease?: boolean;
  /** 清单声明不支持当前平台（os/arch 不含本机）；后端也会在下载前拦截 */
  incompatible?: boolean;
}

interface Props {
  group: {
    id: string;
    displayName: string;
    multiInstance: boolean;
  };
  /** 合并后的完整版本列表（已按版本降序） */
  items: VersionItem[];
  /** 远程目录状态 */
  catalog?: {
    online: boolean;
    cachedAt?: number;
    error?: string;
    loading: boolean;
  };
  onRefresh: () => Promise<void> | void;
  onPick: (item: VersionItem) => Promise<void> | void;
  onUninstall: (version: string) => void;
}

/** 已装版本永远显示在标签上；没有已装版显示最新「正式版」（跳过预发布） */
function summarise(items: VersionItem[], installedCount: number) {
  const installed = items.filter((i) => i.installed);
  if (installed.length === 0) {
    // 预发布可能因版本号更高而排在前面（2.5.0-rc1 > 2.4.66），
    // 但默认展示不该给用户一个 RC —— 优先取最新的正式版，全无正式版才退到 RC
    const headline = items.find((i) => !i.prerelease) ?? items[0];
    return { text: headline?.version ?? "—", sub: null as string | null };
  }
  if (installed.length === 1) {
    const i = installed[0];
    return { text: i.version, sub: i.running ? "on" : null };
  }
  // 多版本共存：显示「N 个版本」+ 使用中版本
  const active = installed.find((i) => i.active);
  return {
    text: `${installedCount} ${"versions"}`,
    sub: active ? active.version : null,
  };
}

export function VersionPicker({ group, items, catalog, onRefresh, onPick, onUninstall }: Props) {
  const t = useT();
  const [open, setOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const [busy, setBusy] = React.useState<string | null>(null);
  const [refreshing, setRefreshing] = React.useState(false);

  const installedCount = items.filter((i) => i.installed).length;
  const headline = summarise(items, installedCount);

  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter(
      (i) => i.version.toLowerCase().includes(q) || (i.note ?? "").toLowerCase().includes(q)
    );
  }, [items, query]);

  // 分组：已装 / 远程可装 / 预发布（分开折叠，主列表保持整洁）
  const groups = React.useMemo(() => {
    const installed = filtered.filter((i) => i.installed);
    const stable = filtered.filter((i) => !i.installed && !i.prerelease);
    const pre = filtered.filter((i) => !i.installed && i.prerelease);
    return [
      { key: "installed", label: t("versions.installed"), items: installed },
      { key: "available", label: t("versions.available"), items: stable },
      { key: "prerelease", label: t("versions.prerelease"), items: pre },
    ].filter((g) => g.items.length > 0);
  }, [filtered, t]);

  const pick = async (item: VersionItem) => {
    if (item.incompatible && !item.installed) {
      return; // UI 已明确标注；真正拦截在后端（PLATFORM_UNSUPPORTED）
    }
    setBusy(item.version);
    try {
      await onPick(item);
      // 安装/切换后保持打开，让用户看到状态变化
    } finally {
      setBusy(null);
    }
  };

  // 最新正式版（用于「最新」徽标；排除预发布）
  const latest = items.find((i) => !i.installed && !i.prerelease)?.version;
  const showRefreshHint = !!catalog?.error;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          className={cn(
            "flex min-w-[6.5rem] items-center gap-2 rounded-full border px-3 py-1.5 text-[12px] transition-colors",
            installedCount > 0
              ? "border-border-strong bg-card-2/60 text-foreground hover:border-primary/50"
              : "border-dashed border-border text-faint hover:border-border-strong hover:text-secondary"
          )}
        >
          {headline.sub && (
            <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-running" aria-hidden />
          )}
          <span className="font-mono">{headline.text}</span>
          {headline.sub && (
            <span className="font-mono text-[10.5px] text-faint">{headline.sub}</span>
          )}
          {catalog?.loading && <Loader2 className="h-3 w-3 animate-spin opacity-50" />}
          <ChevronDown className="h-3 w-3 shrink-0 opacity-50" />
        </button>
      </PopoverTrigger>

      <PopoverContent align="end" className="w-[22rem] p-0">
        {/* 搜索 + 刷新 */}
        <div className="flex items-center gap-1.5 border-b border-border p-2">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("versions.search")}
            className="h-7 flex-1 rounded-md border border-border bg-card px-2 text-[12px] outline-none placeholder:text-faint focus:border-border-strong"
          />
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                onClick={async () => {
                  setRefreshing(true);
                  try {
                    await onRefresh();
                  } finally {
                    setRefreshing(false);
                  }
                }}
                disabled={refreshing}
                className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border text-faint transition-colors hover:border-border-strong hover:text-secondary disabled:opacity-50"
              >
                <RefreshCw className={cn("h-3.5 w-3.5", refreshing && "animate-spin")} />
              </button>
            </TooltipTrigger>
            <TooltipContent>{t("versions.refresh")}</TooltipContent>
          </Tooltip>
        </div>

        {/* 远程状态提示 */}
        {showRefreshHint && (
          <div className="flex items-start gap-1.5 border-b border-border bg-warning-soft/40 px-2.5 py-1.5 text-[10.5px] text-warning">
            <WifiOff className="mt-px h-3 w-3 shrink-0" />
            <span className="leading-snug">{catalog?.error}</span>
          </div>
        )}

        {/* 版本列表 */}
        <div className="max-h-[19rem] overflow-y-auto py-1">
          {groups.length === 0 && (
            <p className="px-3 py-6 text-center text-[12px] text-faint">
              {catalog?.loading ? t("versions.loading") : t("versions.empty")}
            </p>
          )}
          {groups.map((g) => (
            <div key={g.key} className="pb-1">
              <div className="flex items-baseline justify-between px-2.5 pb-1 pt-1.5">
                <span className="text-[10px] font-medium uppercase tracking-wide text-faint">
                  {g.label}
                </span>
                <span className="text-[10px] tabular text-faint">{g.items.length}</span>
              </div>
              <AnimatePresence initial={false}>
                {g.items.map((item) => (
                  <motion.div
                    key={item.version}
                    layout
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    className="group/item relative"
                  >
                    <button
                      onClick={() => pick(item)}
                      disabled={busy !== null || (!!item.incompatible && !item.installed)}
                      title={item.incompatible && !item.installed ? t("versions.incompatible") : undefined}
                      className={cn(
                        "flex w-full items-center gap-2 px-2.5 py-1.5 text-left transition-colors",
                        "hover:bg-fill disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-transparent",
                        item.running && "bg-running-soft/40"
                      )}
                    >
                      {/* 状态图标 */}
                      <span className="flex w-3.5 shrink-0 justify-center">
                        {busy === item.version ? (
                          <Loader2 className="h-3 w-3 animate-spin text-info" />
                        ) : item.running ? (
                          <Power className="h-3 w-3 text-running" />
                        ) : item.installed ? (
                          <Check className="h-3 w-3 text-running opacity-70" />
                        ) : (
                          <Download className="h-3 w-3 text-faint opacity-60" />
                        )}
                      </span>

                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-1.5">
                          <span className="font-mono text-[12px]">{item.version}</span>
                          {item.active && (
                            <span className="rounded-full bg-primary px-1.5 py-px text-[9px] font-medium text-primary-fg">
                              {t("packages.inUse")}
                            </span>
                          )}
                          {item.version === latest && !item.installed && (
                            <span className="rounded-full border border-border px-1.5 py-px text-[9px] text-faint">
                              {t("versions.latest")}
                            </span>
                          )}
                          {item.prerelease && (
                            <span className="rounded-full border border-warning/40 px-1.5 py-px text-[9px] text-warning">
                              {t("versions.pre")}
                            </span>
                          )}
                          {item.incompatible && (
                            <span className="rounded-full border border-error/40 px-1.5 py-px text-[9px] text-error/90">
                              {t("versions.incompatible")}
                            </span>
                          )}
                        </span>
                        {(item.note || item.sizeBytes) && (
                          <span className="mt-0.5 flex items-center gap-1.5 text-[10px] text-faint">
                            {item.note && <span>{item.note}</span>}
                            {item.sizeBytes ? (
                              <span className="tabular">{fmtSize(item.sizeBytes)}</span>
                            ) : null}
                          </span>
                        )}
                      </span>

                      <span className="shrink-0 text-[10px] text-faint opacity-0 transition-opacity group-hover/item:opacity-100">
                        {!item.installed
                          ? t("versions.install")
                          : item.running
                            ? t("versions.stop")
                            : item.active
                              ? t("versions.start")
                              : t("versions.switch")}
                      </span>
                    </button>
                    {item.installed && (
                      <button
                        aria-label={`${t("packages.uninstall")} ${item.version}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          onUninstall(item.version);
                        }}
                        className="absolute right-1.5 top-1.5 hidden h-4 w-4 items-center justify-center rounded-full border border-border bg-surface text-faint transition-colors hover:border-error hover:text-error group-hover/item:flex"
                      >
                        <Trash2 className="h-2 w-2" />
                      </button>
                    )}
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
          ))}
        </div>

        {/* 底部：数据来源 */}
        <div className="flex items-center justify-between border-t border-border px-2.5 py-1.5 text-[10px] text-faint">
          <span>
            {catalog?.online
              ? t("versions.fromRemote")
              : catalog?.cachedAt
                ? t("versions.fromCache")
                : t("versions.fromManifest")}
          </span>
          <span className="tabular">{items.length}</span>
        </div>
      </PopoverContent>
    </Popover>
  );
}

function fmtSize(bytes: number) {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(0)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

export { fmtSize };
