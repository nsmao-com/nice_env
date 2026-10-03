"use client";

import * as React from "react";
import { useIsMutating } from "@tanstack/react-query";
import { motion, AnimatePresence } from "motion/react";
import { Check, ChevronDown, Download, FolderOpen, Loader2, Pin, Power, RefreshCw, Trash2, WifiOff, X } from "lucide-react";
import type { RemoteVersion } from "@nsb/schema";
import { cn, sameVersion } from "@/lib/utils";
import { useT } from "@/lib/store";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";
import { PathEnvToggle } from "@/components/shared/path-env-toggle";

/** 下拉里的一项：清单内置版本与远程版本合并后的统一视图 */
export interface VersionItem {
  version: string;
  installed: boolean;
  /** 已安装运行时目录，桌面端可直接在系统文件管理器中打开 */
  installPath?: string;
  /** 默认版本；与服务是否正在运行独立。 */
  active: boolean;
  running: boolean;
  canStop?: boolean;
  transitioning?: boolean;
  installing?: boolean;
  /** 上游目录中该版本的下载信息，也可能与内置清单重合。 */
  remote?: RemoteVersion;
  sizeBytes?: number;
  note?: string;
  prerelease?: boolean;
  /** 清单声明不支持当前平台（os/arch 不含本机）；后端也会在下载前拦截 */
  incompatible?: boolean;
}

export type VersionAction = "install" | "active" | "start" | "stop";

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
  onPick: (item: VersionItem, action: VersionAction, trigger: HTMLButtonElement | null) => Promise<void> | void;
  onOpenFolder: (item: VersionItem) => Promise<void> | void;
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

export function VersionPicker({ group, items, catalog, disabled = false, statusKnown = true, onRefresh, onPick, onOpenFolder, onUninstall }: Props) {
  const t = useT();
  const pathBusy = useIsMutating({ mutationKey: ["pathenv-change"] }) > 0;
  const [open, setOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const [busy, setBusy] = React.useState<string | null>(null);
  const busyRef = React.useRef(false);
  const triggerRef = React.useRef<HTMLButtonElement>(null);
  const searchRef = React.useRef<HTMLInputElement>(null);
  const dialogHandoff = React.useRef(false);
  const refreshRef = React.useRef(false);
  const [refreshing, setRefreshing] = React.useState(false);
  const [failure, setFailure] = React.useState<{ version: string; action: VersionAction; error: AppErrorShape } | null>(null);
  const failureRef = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    if (open && failure) failureRef.current?.scrollIntoView({ block: "nearest" });
  }, [open, failure]);

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

  // 已安装状态优先；内置历史版本独立展示，不把目录未收录等同于已撤下。
  const groups = React.useMemo(() => {
    const installed = filtered.filter((i) => i.installed);
    const stable = filtered.filter((i) => !i.installed && i.remote && !i.prerelease);
    const pre = filtered.filter((i) => !i.installed && i.remote && i.prerelease);
    const manifest = filtered.filter((i) => !i.installed && !i.remote);
    return [
      { key: "installed", label: t("versions.installed"), items: installed },
      { key: "available", label: t("versions.available"), items: stable },
      { key: "prerelease", label: t("versions.prerelease"), items: pre },
      { key: "manifest", label: t("versions.fromManifest"), items: manifest },
    ].filter((g) => g.items.length > 0);
  }, [filtered, t]);

  const actionOf = (item: VersionItem): VersionAction => !item.installed ? "install"
    : !group.isService ? "active"
    : item.canStop || item.running ? "stop"
    : item.active || group.multiInstance ? "start" : "active";
  const actionLabel = (action: VersionAction) => action === "active"
    ? t(group.multiInstance ? "versions.setDefault" : "versions.switch") : t(`versions.${action}`);
  const retryItem = failure ? items.find((item) => sameVersion(item.version, failure.version)) : undefined;

  const pick = async (item: VersionItem, action: VersionAction = actionOf(item)) => {
    const needsServiceStatus = group.isService && action !== "install";
    if (busyRef.current || pathBusy || disabled || (needsServiceStatus && !statusKnown) || item.installing || item.transitioning) return;
    if (item.incompatible && !item.installed) {
      return; // UI 已明确标注；真正拦截在后端（PLATFORM_UNSUPPORTED）
    }
    busyRef.current = true;
    setBusy(item.version);
    setFailure(null);
    const opensInstall = action === "install" && !item.installed;
    try {
      if (opensInstall) {
        dialogHandoff.current = true;
        setOpen(false);
      }
      await onPick(item, action, triggerRef.current);
      // 切换/启停保持打开；安装交给独立弹窗，避免两个浮层争抢焦点。
    } catch (error) {
      if (opensInstall) { dialogHandoff.current = false; setOpen(true); }
      setFailure({ version: item.version, action, error: normalizeError(error) });
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  // 最新正式版（用于「最新」徽标；排除预发布）
  const latest = items.find((i) => !i.prerelease && !i.incompatible)?.version;
  const showRefreshHint = !!catalog?.error;

  return (
    <Popover open={open} onOpenChange={(next) => { if (!busyRef.current && !pathBusy) setOpen(next); }}>
      <PopoverTrigger asChild>
        <button
          ref={triggerRef}
          aria-label={`${group.displayName} ${t("versions.manage")} · ${headline.text}`}
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
            <span className="font-mono text-[10.5px] text-muted">{headline.sub}</span>
          )}
          {catalog?.loading && <Loader2 className="h-3 w-3 animate-spin opacity-50" />}
          <ChevronDown className="h-3 w-3 shrink-0 opacity-50" />
        </button>
      </PopoverTrigger>

      <PopoverContent align="end" collisionPadding={12}
        aria-label={`${group.displayName} ${t("versions.manage")}`}
        onCloseAutoFocus={(event) => {
          if (dialogHandoff.current) {
            event.preventDefault();
            dialogHandoff.current = false;
          }
        }}
        className="flex max-h-[var(--radix-popover-content-available-height)] w-[22rem] max-w-[calc(100vw-24px)] flex-col overflow-hidden p-0">
        {/* 搜索 + 刷新 */}
        <div className="flex shrink-0 items-center gap-1.5 p-2">
          <div className="relative min-w-0 flex-1">
            <input
              ref={searchRef}
              value={query}
              disabled={busy !== null}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("versions.search")}
              aria-label={t("versions.search")}
              className="h-8 w-full min-w-0 rounded-md border border-border bg-card pl-2 pr-8 text-[12px] outline-none placeholder:text-muted focus:border-primary"
            />
            {query && <button type="button" aria-label={t("versions.clearSearch")} disabled={busy !== null}
              onClick={() => { setQuery(""); searchRef.current?.focus(); }}
              className="absolute inset-y-0 right-0 flex w-8 items-center justify-center rounded-md text-muted hover:bg-fill hover:text-foreground focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary disabled:opacity-50">
              <X className="h-3.5 w-3.5" />
            </button>}
          </div>
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

        {/* 搜索和来源固定；列表、提示和错误一起滚动，矮窗口也能操作版本。 */}
        <div className="min-h-0 max-h-[19rem] overflow-y-auto overscroll-contain py-1">
        {/* 远程状态提示 */}
        {showRefreshHint && (
          <>
            <div role="status" className="flex items-start gap-1.5 bg-warning-soft/40 px-2.5 py-1.5 text-[10.5px] text-warning [overflow-wrap:anywhere]">
              <WifiOff className="mt-px h-3 w-3 shrink-0" />
              <span className="leading-snug">{catalog?.error}</span>
            </div>
            <div role="separator" className="mx-3 border-t border-dashed border-separator" />
          </>
        )}

        {/* 版本列表 */}
          {groups.length === 0 && (
            <p role="status" className="px-3 py-4 text-center text-[12px] text-faint">
              {catalog?.loading ? t("versions.loading") : t(query.trim() ? "versions.empty" : "versions.noVersions")}
            </p>
          )}
          {groups.map((g, index) => (
            <div key={g.key} role="group" aria-label={g.label} className="pb-1">
              {index > 0 && <div role="separator" className="mx-3 my-1 border-t border-dashed border-separator" />}
              <div className="flex items-baseline justify-between px-2.5 pb-1 pt-1.5">
                <span className="text-[10px] font-medium uppercase tracking-wide text-muted">
                  {g.label}
                </span>
                <span className="text-[10px] tabular text-muted">{g.items.length}</span>
              </div>
              {g.key === "manifest" && !!catalog?.cachedAt && (
                <p className="px-2.5 pb-2 text-[10.5px] leading-relaxed text-muted [overflow-wrap:anywhere]">{t("versions.manifestOnlyHint")}</p>
              )}
              <AnimatePresence initial={false}>
                {g.items.map((item) => (
                  <motion.div
                    key={item.version}
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    className={cn("group/item mx-2 mb-1.5 flex flex-wrap items-stretch overflow-hidden rounded-xl border border-transparent p-1 transition-colors hover:border-border hover:bg-fill", item.installed && "border-border/60 bg-card-2/30", statusKnown && item.running && "border-running/20 bg-running-soft/40")}
                  >
                    <button
                      onClick={() => pick(item)}
                      disabled={disabled || (group.isService && !statusKnown && actionOf(item) !== "install") || pathBusy || busy !== null || item.installing || item.transitioning || (!!item.incompatible && !item.installed)
                        || (!group.isService && item.installed && item.active)}
                      title={item.incompatible && !item.installed ? t("versions.incompatible") : undefined}
                      className={cn(
                        "flex min-h-10 min-w-0 flex-1 items-center gap-2 px-2.5 py-1.5 text-left transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary",
                        "rounded-lg disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-transparent",
                        !group.isService && item.installed && item.active && "disabled:cursor-default disabled:opacity-100",
                        statusKnown && item.running && "text-foreground"
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
                          {sameVersion(item.version, latest) && !item.installed && (
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
                          <span className="mt-0.5 flex items-center gap-1.5 text-[10px] text-muted">
                            {item.note && <span>{item.note}</span>}
                            {item.sizeBytes ? (
                              <span className="tabular">{fmtSize(item.sizeBytes)}</span>
                            ) : null}
                          </span>
                        )}
                      </span>

                      <span className="shrink-0 text-[11px] text-foreground">
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
                      <div className="flex w-full flex-wrap items-center gap-1.5 px-2.5 pb-1">
                        <PathEnvToggle pkgId={group.id} version={item.version} disabled={disabled || busy !== null || !!item.installing} />
                        {group.multiInstance && <button
                          type="button"
                          aria-label={`${t("versions.setDefault")} ${group.displayName} ${item.version}`}
                          disabled={disabled || !statusKnown || pathBusy || busy !== null || item.installing || item.transitioning || item.active}
                          onClick={() => void pick(item, "active")}
                          className="inline-flex min-h-7 items-center gap-1.5 rounded-md px-2 text-[11px] text-muted transition-colors hover:bg-fill hover:text-foreground focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary disabled:opacity-50"
                        >
                          {item.active ? <Check className="h-3 w-3" /> : <Pin className="h-3 w-3" />}
                          {t(item.active ? "versions.default" : "versions.setDefault")}
                        </button>}
                        <span className="flex-1" />

                        <button
                          type="button"
                          aria-label={`${t("packages.openFolder")} ${group.displayName} ${item.version}`}
                          title={t("packages.openFolder")}
                          disabled={disabled || busy !== null || item.installing || !item.installPath}
                          onClick={(event) => {
                            event.stopPropagation();
                            void onOpenFolder(item);
                          }}
                          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted transition-colors hover:bg-fill hover:text-foreground focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary disabled:opacity-45"
                        >
                          <FolderOpen className="h-3.5 w-3.5" />
                        </button>

                      <button
                        aria-label={`${t("packages.uninstall")} ${item.version}`}
                        disabled={disabled || pathBusy || busy !== null || item.installing}
                        onClick={(e) => {
                          e.stopPropagation();
                          dialogHandoff.current = true;
                          setOpen(false);
                          onUninstall(item.version, triggerRef.current);
                        }}
                        className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted transition-colors hover:bg-error-soft hover:text-error focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary disabled:opacity-45"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>

                      </div>
                    )}
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
          ))}

        {failure && (
          <div ref={failureRef} role="alert" className="mx-2.5 mb-2 rounded-lg bg-error-soft p-2 text-[11px] text-error [overflow-wrap:anywhere]">
            <p className="mb-1 font-medium">{actionLabel(failure.action)} · {failure.version}</p>
            <p>{failure.error.message}</p>
            {failure.error.hint && <p className="mt-1">{failure.error.hint}</p>}
            {!retryItem && <p className="mt-1">{t("versions.unavailable")}</p>}
            <button type="button" disabled={disabled || !statusKnown || pathBusy || busy !== null || !retryItem || retryItem.installing || retryItem.transitioning || (!!retryItem.incompatible && !retryItem.installed)} onClick={() => { if (retryItem) void pick(retryItem, failure.action); }} className="mt-1.5 rounded-md border border-error/30 px-2 py-1 focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary disabled:opacity-50">{t("bulk.retry")} · {actionLabel(failure.action)}</button>
          </div>
        )}
        {installedCount > 0 && (
          <p className="px-2.5 pb-2 text-[10px] leading-relaxed text-muted">{t("tools.pathEnvOnlyActive")}{group.multiInstance ? ` ${t("versions.defaultHint")}` : ""}</p>
        )}
        </div>

        {/* 底部：数据来源 */}
        <div role="separator" className="mx-3 border-t border-dashed border-separator" />
        <div className="flex shrink-0 items-center justify-between gap-2 px-2.5 py-1.5 text-[10px] text-muted">
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
          <span className="shrink-0 tabular" aria-label={query.trim() ? `${t("versions.matches")} ${filtered.length} / ${items.length}` : undefined}>
            {query.trim() ? `${filtered.length} / ${items.length}` : items.length}
          </span>
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
