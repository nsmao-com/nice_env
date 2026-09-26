"use client";

import * as React from "react";
import { motion, AnimatePresence } from "motion/react";
import { Check, ChevronDown, Download, Loader2, Pin, Power, RefreshCw, Trash2, WifiOff } from "lucide-react";
import type { RemoteVersion } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

/** 下拉里的一项：清单内置版本与远程版本合并后的统一视图 */
export interface VersionItem {
  version: string;
  installed: boolean;
  /** 默认版本；与服务是否正在运行独立。 */
  active: boolean;
  running: boolean;
  canStop?: boolean;
  transitioning?: boolean;
  installing?: boolean;
  /** 来自远程枚举（清单里没有） */
  remote?: RemoteVersion;
  sizeBytes?: number;
  note?: string;
  prerelease?: boolean;
  /** 清单声明不支持当前平台（os/arch 不含本机）；后端也会在下载前拦截 */
  incompatible?: boolean;
}

interface Props {
  disabled?: boolean;
  statusKnown?: boolean;
  group: {
    id: string;
    displayName: string;
    multiInstance: boolean;
    isService: boolean;
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
  onSetActive: (item: VersionItem) => Promise<void>;
  onUninstall: (version: string, trigger: HTMLButtonElement | null) => void;
}

/** 已装版本永远显示在标签上；没有已装版显示最新「正式版」（跳过预发布） */
function summarise(items: VersionItem[], countLabel: string) {
  const installed = items.filter((i) => i.installed);
  if (installed.length === 0) {
    // 预发布可能因版本号更高而排在前面（2.5.0-rc1 > 2.4.66），
    // 但默认展示不该给用户一个 RC —— 优先取最新的正式版，全无正式版才退到 RC
    const headline = items.find((i) => !i.prerelease && !i.incompatible) ?? items[0];
    return { text: headline?.version ?? "—", sub: null as string | null };
  }
  if (installed.length === 1) {
    const i = installed[0];
    return { text: i.version, sub: null };
  }
  // 多版本共存：显示「N 个版本」+ 使用中版本
  const active = installed.find((i) => i.active);
  return {
    text: countLabel,
    sub: active ? active.version : null,
  };
}

export function VersionPicker({ group, items, catalog, disabled = false, statusKnown = true, onRefresh, onPick, onSetActive, onUninstall }: Props) {
  const t = useT();
  const [open, setOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const [busy, setBusy] = React.useState<string | null>(null);
  const busyRef = React.useRef(false);
  const triggerRef = React.useRef<HTMLButtonElement>(null);
  const uninstallHandoff = React.useRef(false);
  const refreshRef = React.useRef(false);
  const [refreshing, setRefreshing] = React.useState(false);
  const [failure, setFailure] = React.useState<{ item: VersionItem; action: "pick" | "active"; error: AppErrorShape } | null>(null);

  const installedCount = items.filter((i) => i.installed).length;
  const headline = summarise(items, `${installedCount} ${t("versions.countUnit")}`);
  const anyRunning = statusKnown && items.some((item) => item.running);

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

  const pick = async (item: VersionItem, action: "pick" | "active" = "pick") => {
    if (busyRef.current || disabled || item.installing) return;
    if (item.incompatible && !item.installed) {
      return; // UI 已明确标注；真正拦截在后端（PLATFORM_UNSUPPORTED）
    }
    busyRef.current = true;
    setBusy(item.version);
    setFailure(null);
    try {
      if (action === "active") await onSetActive(item);
      else await onPick(item);
      // 安装/切换后保持打开，让用户看到状态变化
    } catch (error) {
      setFailure({ item, action, error: normalizeError(error) });
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  // 最新正式版（用于「最新」徽标；排除预发布）
  const latest = items.find((i) => !i.prerelease && !i.incompatible)?.version;
  const showRefreshHint = !!catalog?.error;

  return (
    <Popover open={open} onOpenChange={(next) => { if (!busyRef.current) setOpen(next); }}>
      <PopoverTrigger asChild>
        <button
          ref={triggerRef}
          aria-label={`${group.displayName} ${headline.text} ${t("versions.available")}`}
          className={cn(
            "flex min-w-[6.5rem] max-w-full shrink-0 items-center gap-2 whitespace-nowrap rounded-full border px-3 py-1.5 text-[12px] transition-colors",
            installedCount > 0
              ? "border-border-strong bg-card-2/60 text-foreground hover:border-primary/50"
              : "border-dashed border-border text-faint hover:border-border-strong hover:text-secondary"
          )}
        >
          {anyRunning && (
            <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-running" aria-hidden />
          )}
          <span className="min-w-0 truncate font-mono">{headline.text}</span>
          {headline.sub && (
            <span className="font-mono text-[10.5px] text-faint">{headline.sub}</span>
          )}
          {catalog?.loading && <Loader2 className="h-3 w-3 animate-spin opacity-50" />}
          <ChevronDown className="h-3 w-3 shrink-0 opacity-50" />
        </button>
      </PopoverTrigger>

      <PopoverContent align="end" collisionPadding={12}
        onCloseAutoFocus={(event) => {
          if (uninstallHandoff.current) {
            event.preventDefault();
            uninstallHandoff.current = false;
          }
        }}
        className="flex max-h-[var(--radix-popover-content-available-height)] w-[22rem] max-w-[calc(100vw-24px)] flex-col overflow-hidden p-0">
        {/* 搜索 + 刷新 */}
        <div className="flex shrink-0 items-center gap-1.5 p-2">
          <input
            value={query}
            disabled={busy !== null}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("versions.search")}
            aria-label={t("versions.search")}
            className="h-8 min-w-0 flex-1 rounded-md border border-border bg-card px-2 text-[12px] outline-none placeholder:text-faint focus:border-primary"
          />
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                aria-label={t("versions.refresh")}
                onClick={async () => {
                  if (refreshRef.current) return;
                  refreshRef.current = true;
                  setRefreshing(true);
                  try {
                    await onRefresh();
                  } finally {
                    refreshRef.current = false;
                    setRefreshing(false);
                  }
                }}
                disabled={!isTauri || busy !== null || refreshing || catalog?.loading}
                className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border text-faint transition-colors hover:border-border-strong hover:text-secondary disabled:opacity-50"
              >
                <RefreshCw className={cn("h-3.5 w-3.5", refreshing && "animate-spin")} />
              </button>
            </TooltipTrigger>
            <TooltipContent>{t(isTauri ? "versions.refresh" : "versions.preview")}</TooltipContent>
          </Tooltip>
        </div>
        <div role="separator" className="mx-3 border-t border-dashed border-separator" />

        {/* 远程状态提示 */}
        {showRefreshHint && (
          <>
            <div className="flex items-start gap-1.5 bg-warning-soft/40 px-2.5 py-1.5 text-[10.5px] text-warning">
              <WifiOff className="mt-px h-3 w-3 shrink-0" />
              <span className="leading-snug">{catalog?.error}</span>
            </div>
            <div role="separator" className="mx-3 border-t border-dashed border-separator" />
          </>
        )}

        {/* 版本列表 */}
        <div className="min-h-0 max-h-[19rem] overflow-y-auto py-1">
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
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    className="group/item flex flex-wrap items-stretch"
                  >
                    <button
                      onClick={() => pick(item)}
                      disabled={disabled || busy !== null || item.installing || item.transitioning || (!!item.incompatible && !item.installed)
                        || (!group.isService && item.installed && item.active)}
                      title={item.incompatible && !item.installed ? t("versions.incompatible") : undefined}
                      className={cn(
                        "flex min-h-10 min-w-0 flex-1 items-center gap-2 px-2.5 py-1.5 text-left transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary",
                        "hover:bg-fill disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-transparent",
                        !group.isService && item.installed && item.active && "disabled:cursor-default disabled:opacity-100",
                        statusKnown && item.running && "bg-running-soft/40"
                      )}
                    >
                      {/* 状态图标 */}
                      <span className="flex w-3.5 shrink-0 justify-center">
                        {busy === item.version ? (
                          <Loader2 className="h-3 w-3 animate-spin text-info" />
                        ) : statusKnown && item.running ? (
                          <Power className="h-3 w-3 text-running" />
                        ) : item.installed ? (
                          <Check className="h-3 w-3 text-running opacity-70" />
                        ) : (
                          <Download className="h-3 w-3 text-faint opacity-60" />
                        )}
                      </span>

                      <span className="min-w-0 flex-1">
                        <span className="flex flex-wrap items-center gap-1.5">
                          <span className="break-all font-mono text-[12px]">{item.version}</span>
                          {item.active && (
                            <span className="rounded-full bg-primary px-1.5 py-px text-[9px] font-medium text-primary-fg">
                              {t(group.multiInstance || !group.isService ? "versions.default" : "packages.inUse")}
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
                        {!!(item.note || item.sizeBytes) && (
                          <span className="mt-0.5 flex items-center gap-1.5 text-[10px] text-faint">
                            {item.note && <span>{item.note}</span>}
                            {item.sizeBytes ? (
                              <span className="tabular">{fmtSize(item.sizeBytes)}</span>
                            ) : null}
                          </span>
                        )}
                      </span>

                      <span className="shrink-0 text-[10px] text-faint">
                        {item.installing ? t("packages.installing")
                          : !statusKnown && item.installed && group.isService ? t("packages.statusUnknown")
                          : !item.installed
                          ? t("versions.install")
                          : !group.isService
                            ? item.active ? null : t("versions.switch")
                          : item.transitioning
                            ? t("common.loading")
                          : item.canStop
                            ? t("versions.stop")
                            : item.active || group.multiInstance
                              ? t("versions.start")
                              : t("versions.switch")}
                      </span>
                    </button>
                    {item.installed && (
                      <button
                        aria-label={`${t("packages.uninstall")} ${item.version}`}
                        disabled={disabled || busy !== null || item.installing}
                        onClick={(e) => {
                          e.stopPropagation();
                          uninstallHandoff.current = true;
                          setOpen(false);
                          onUninstall(item.version, triggerRef.current);
                        }}
                        className="flex w-10 shrink-0 items-center justify-center text-faint transition-colors hover:bg-error-soft hover:text-error focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary disabled:opacity-45"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    )}
                    {item.installed && group.multiInstance && (
                      <div className="w-full px-2.5 pb-1.5">
                        <button
                          type="button"
                          aria-label={`${t("versions.setDefault")} ${group.displayName} ${item.version}`}
                          disabled={disabled || busy !== null || item.installing || item.active}
                          onClick={() => void pick(item, "active")}
                          className="inline-flex min-h-7 items-center gap-1.5 rounded-md px-2 text-[11px] text-muted transition-colors hover:bg-fill hover:text-foreground focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary disabled:opacity-50"
                        >
                          {item.active ? <Check className="h-3 w-3" /> : <Pin className="h-3 w-3" />}
                          {t(item.active ? "versions.default" : "versions.setDefault")}
                        </button>
                      </div>
                    )}
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
          ))}
        </div>

        {failure && (
          <div role="alert" className="mx-2.5 mb-2 max-h-32 shrink-0 overflow-y-auto rounded-lg bg-error-soft p-2 text-[11px] text-error [overflow-wrap:anywhere]">
            <p>{failure.error.message}</p>
            {failure.error.hint && <p className="mt-1">{failure.error.hint}</p>}
            <button type="button" disabled={disabled || busy !== null || items.find((i) => i.version === failure.item.version)?.installing} onClick={() => void pick(failure.item, failure.action)} className="mt-1.5 rounded-md border border-error/30 px-2 py-1 focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary">{t("bulk.retry")}</button>
          </div>
        )}
        {group.multiInstance && installedCount > 0 && (
          <p className="shrink-0 px-2.5 pb-2 text-[10px] leading-relaxed text-muted">{t("versions.defaultHint")}</p>
        )}

        {/* 底部：数据来源 */}
        <div role="separator" className="mx-3 border-t border-dashed border-separator" />
        <div className="flex shrink-0 items-center justify-between gap-2 px-2.5 py-1.5 text-[10px] text-faint">
          <span>
            {!isTauri
              ? t("versions.preview")
              : catalog?.loading
                ? t("versions.loading")
                : catalog?.online
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
