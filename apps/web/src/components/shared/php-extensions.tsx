"use client";

import * as React from "react";
import { motion } from "motion/react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Check,
  Loader2,
  Puzzle,
  RotateCw,
  Search,
  ShieldAlert,
  Sparkles,
  Bug,
  CircleAlert,
  CircleCheck,
  FolderOpen,
} from "lucide-react";
import type { PhpExtension, PhpExtensionView, XdebugStatus } from "@nsb/schema";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Skeleton } from "@/components/ui/misc";

/**
 * PHP 扩展面板。
 *
 * 对标 ServBay / FlyEnv / phpStudy 的扩展管理：勾选即启用，改完立刻生效。
 * 三件容易被忽略但很关键的事，这里都做了：
 * 1. **按真实磁盘扫描**——列出来的一定是 ext/ 目录里存在的 DLL，不会给一个
 *    永远装不上的名字；
 * 2. **改完实测**——用 `php -n -c <ini> -m` 跑一遍，加载失败把 PHP 的原始
 *    告警直接摊给用户看，而不是让他对着一个「已启用」的假状态；
 * 3. **顺带重启**——PHP 正在运行时自动重启 php-cgi，否则勾了没反应。
 */

/** 后端下发的分组是稳定 key（Rust phpext.rs），文案在这里本地化 */
const GROUP_KEYS = [
  "basic",
  "database",
  "cache",
  "network",
  "text",
  "image",
  "archive",
  "performance",
  "debug",
  "security",
  "file",
  "system",
  "other",
] as const;

/** key → 当前语言标签；未知 key（前向兼容旧清单）原样展示 */
function groupLabel(group: string, t: (k: never) => string) {
  if ((GROUP_KEYS as readonly string[]).includes(group)) {
    return t(`phpext.group.${group}` as never);
  }
  return group;
}
export function PhpExtensionsDialog({
  version,
  open,
  onOpenChange,
}: {
  version: string | null;
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const [view, setView] = React.useState<PhpExtensionView | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [busy, setBusy] = React.useState<string | null>(null);
  const [query, setQuery] = React.useState("");
  const [onlyEnabled, setOnlyEnabled] = React.useState(false);

  const load = React.useCallback(async () => {
    if (!version) return;
    setLoading(true);
    try {
      setView(await api.phpExtensions(version));
    } catch (e) {
      toastError(e);
    } finally {
      setLoading(false);
    }
  }, [version]);

  React.useEffect(() => {
    if (open && version) {
      setQuery("");
      setOnlyEnabled(false);
      void load();
    }
  }, [open, version, load]);

  const toggle = async (ext: PhpExtension, next: boolean) => {
    if (!version) return;
    setBusy(ext.name);
    try {
      const r = await api.setPhpExtension(version, ext.name, next);
      // 乐观更新本地状态，避免整表闪烁
      setView((v) =>
        v
          ? {
              ...v,
              extensions: v.extensions.map((e) =>
                e.name === ext.name ? { ...e, enabled: next } : e
              ),
            }
          : v
      );
      if (r.warnings.length > 0) {
        toast.warning(
          `${ext.label} ${t("phpext.writtenWithWarnings")}`,
          { description: r.warnings.slice(0, 3).join("\n"), duration: 9000 }
        );
      } else if (r.needsRestart) {
        toast.info(`${ext.label} ${next ? t("phpext.enabledToast") : t("phpext.disabledToast")}`, {
          description: t("phpext.needsRestart"),
        });
      } else {
        toast.success(`${ext.label} ${next ? t("phpext.enabledToast") : t("phpext.disabledToast")}`);
      }
      void load();
      invalidate("services");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  const toggleIni = async (key: string, next: boolean) => {
    if (!version) return;
    setView((v) =>
      v ? { ...v, toggles: v.toggles.map((x) => (x.key === key ? { ...x, value: next } : x)) } : v
    );
    try {
      await api.setPhpIniToggle(version, key, next);
      toast.success(t("phpext.iniSaved"));
    } catch (e) {
      // 失败要回滚，不然界面显示的是没生效的状态
      setView((v) =>
        v ? { ...v, toggles: v.toggles.map((x) => (x.key === key ? { ...x, value: !next } : x)) } : v
      );
      toastError(e);
    }
  };

  const filtered = React.useMemo(() => {
    if (!view) return [];
    const q = query.trim().toLowerCase();
    return view.extensions.filter((e) => {
      if (onlyEnabled && !e.enabled) return false;
      if (!q) return true;
      return (
        e.name.toLowerCase().includes(q) ||
        e.label.toLowerCase().includes(q) ||
        groupLabel(e.group, t).toLowerCase().includes(q) ||
        e.hint.toLowerCase().includes(q)
      );
    });
  }, [view, query, onlyEnabled, t]);

  // 按分组归拢，顺序固定，避免每次刷新跳来跳去
  // 后端只发稳定 key（basic/database…），文案本地化在前端做
  const grouped = React.useMemo(() => {
    const order = GROUP_KEYS;
    const map = new Map<string, PhpExtension[]>();
    for (const e of filtered) {
      const arr = map.get(e.group) ?? [];
      arr.push(e);
      map.set(e.group, arr);
    }
    return order
      .filter((g) => map.has(g))
      .map((g) => [g, map.get(g)!] as const)
      .concat(
        Array.from(map.keys())
          .filter((g) => !order.includes(g))
          .map((g) => [g, map.get(g)!] as const)
      );
  }, [filtered]);

  const enabledCount = view?.extensions.filter((e) => e.enabled).length ?? 0;
  const totalCount = view?.extensions.length ?? 0;
  const depsIssue = view?.extensions.filter((e) => e.enabled && e.missingDeps.length > 0) ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[85vh] max-w-3xl flex-col gap-0 overflow-hidden p-0">
        <DialogHeader className="shrink-0 border-b border-border px-5 py-4">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-primary/30 bg-primary-soft">
              <Puzzle className="h-[18px] w-[18px] text-primary" strokeWidth={1.8} />
            </div>
            <div className="min-w-0 flex-1">
              <DialogTitle className="text-[15px]">
                {t("phpext.title")}
                <span className="ml-2 font-mono text-[12px] font-normal text-muted">PHP {version}</span>
              </DialogTitle>
              <DialogDescription className="mt-0.5 text-[11.5px]">
                {loading ? t("common.loading") : `${enabledCount}/${totalCount} ${t("phpext.enabledCount")}`}
                {view?.iniPath ? (
                  <span className="ml-2 font-mono text-[10.5px] text-faint">{view.iniPath}</span>
                ) : null}
              </DialogDescription>
            </div>
            <Button
              size="sm"
              variant="ghost"
              className="shrink-0"
              onClick={() => void load()}
              disabled={loading}
            >
              <RotateCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
            </Button>
          </div>

          {/* 搜索 + 只看已启用 */}
          <div className="mt-3 flex items-center gap-2">
            <div className="relative flex-1">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-faint" />
              <Input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("phpext.searchPlaceholder")}
                className="h-8 pl-8 text-[12.5px]"
              />
            </div>
            <Button
              size="sm"
              variant={onlyEnabled ? "secondary" : "ghost"}
              className="h-8 shrink-0 text-[12px]"
              onClick={() => setOnlyEnabled((v) => !v)}
            >
              {t("phpext.onlyEnabled")}
            </Button>
          </div>
        </DialogHeader>

        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {/* 依赖缺失提示：比让用户去猜「为什么 redis 装上没用」友好得多 */}
          {depsIssue.length > 0 && (
            <div className="mb-4 flex items-start gap-2.5 rounded-xl border border-warn/25 bg-warn-soft p-3">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-warn" strokeWidth={2} />
              <div className="text-[12px] leading-relaxed">
                <div className="font-medium">{t("phpext.depsTitle")}</div>
                <ul className="mt-1 space-y-0.5 text-muted">
                  {depsIssue.slice(0, 4).map((e) => (
                    <li key={e.name}>
                      <span className="font-mono">{e.name}</span> {t("phpext.depsNeeds")}{" "}
                      <span className="font-mono">{e.missingDeps.join(", ")}</span>
                    </li>
                  ))}
                </ul>
              </div>
            </div>
          )}

          {loading && !view ? (
            <div className="space-y-2">
              {Array.from({ length: 8 }).map((_, i) => (
                <Skeleton key={i} className="h-11 w-full" />
              ))}
            </div>
          ) : filtered.length === 0 ? (
            <div className="py-12 text-center text-[13px] text-muted">
              {query ? t("phpext.noMatch") : t("phpext.empty")}
            </div>
          ) : (
            <div className="space-y-5">
              {grouped.map(([group, items]) => (
                <div key={group}>
                  <div className="mb-2 flex items-center gap-2">
                    <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                      {group}
                    </span>
                    <span className="h-px flex-1 bg-border/60" />
                  </div>
                  <div className="space-y-1.5">
                    {items.map((e) => (
                      <ExtRow
                        key={e.name}
                        ext={e}
                        busy={busy === e.name}
                        onToggle={(next) => void toggle(e, next)}
                      />
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Xdebug 一键调试：ServBay 的招牌功能，这里做成面板内的独立卡片 */}
          {version && <XdebugCard version={version} onChanged={() => void load()} />}

          {/* php.ini 快捷开关 */}
          {view && view.toggles.length > 0 && (
            <div className="mt-6">
              <div className="mb-2 flex items-center gap-2">
                <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                  {t("phpext.quickToggles")}
                </span>
                <span className="h-px flex-1 bg-border/60" />
              </div>
              <div className="space-y-1.5">
                {view.toggles.map((tg) => (
                  <div
                    key={tg.key}
                    className="flex items-center gap-3 rounded-lg border border-border/70 bg-card-2/30 px-3 py-2"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="text-[12.5px] font-medium">{tg.label}</span>
                        <span className="font-mono text-[10.5px] text-faint">{tg.key}</span>
                      </div>
                      <p className="truncate text-[11px] text-faint">{tg.hint}</p>
                    </div>
                    <Switch
                      checked={tg.value}
                      onCheckedChange={(v) => void toggleIni(tg.key, v)}
                      aria-label={tg.label}
                    />
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Xdebug 一键配置卡片。
 *
 * 三态清晰可见，因为这个功能的失败模式很隐蔽：
 * - 装了 DLL 但 php.ini 没启用 → 显示「未启用」
 * - 启用了但 DLL 构建指纹不匹配 → PHP 会拒绝加载，这里如实显示「未加载成功」
 * - 真的加载上了 → 显示实测到的版本号（不是我们以为写进去的版本）
 */
function XdebugCard({ version, onChanged }: { version: string; onChanged: () => void }) {
  const t = useT();
  const [st, setSt] = React.useState<XdebugStatus | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [mode, setMode] = React.useState("debug,develop");
  const [port, setPort] = React.useState(9003);

  const load = React.useCallback(async () => {
    try {
      const v = await api.xdebugStatus(version);
      setSt(v);
      const m = v.settings["xdebug.mode"];
      if (m) setMode(m);
      const p = v.settings["xdebug.client_port"];
      if (p) setPort(Number(p) || 9003);
    } catch {
      setSt(null);
    }
  }, [version]);

  React.useEffect(() => {
    void load();
  }, [load]);

  const setup = async () => {
    setBusy(true);
    try {
      const r = await api.xdebugSetup({ version, mode, clientPort: port });
      if (r.installed) {
        toast.success(t("xdebug.ready"), {
          description: r.loadedVersion ? `Xdebug ${r.loadedVersion}` : undefined,
        });
      } else if (r.manualHint) {
        toast.warning(t("xdebug.autofail"), { description: r.manualHint, duration: 12000 });
      } else {
        toast.error(t("xdebug.failed"), {
          description: r.warnings.slice(0, 3).join("\n"),
          duration: 10000,
        });
      }
      await load();
      onChanged();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      const w = await api.xdebugToggle(version, next, mode, port);
      if (w.length) toast.warning(w.join("\n"));
      else toast.success(next ? t("xdebug.enabled") : t("xdebug.disabled"));
      await load();
      onChanged();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  if (!st) return null;

  const healthy = st.loaded;
  return (
    <div className="mt-6">
      <div className="mb-2 flex items-center gap-2">
        <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
          {t("xdebug.title")}
        </span>
        <span className="h-px flex-1 bg-border/60" />
      </div>

      <div className="rounded-xl border border-border/70 bg-card-2/30 p-3.5">
        <div className="flex items-start gap-3">
          <div
            className={cn(
              "flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border",
              healthy ? "border-running/30 bg-running-soft" : "border-border bg-card-2/60"
            )}
          >
            <Bug className={cn("h-4 w-4", healthy ? "text-running" : "text-faint")} strokeWidth={1.8} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-[12.5px] font-medium">Xdebug</span>
              {healthy ? (
                <span className="inline-flex items-center gap-1 text-[11px] text-running">
                  <CircleCheck className="h-3.5 w-3.5" />
                  {t("xdebug.loaded")} {st.loadedVersion}
                </span>
              ) : st.enabled ? (
                <span className="inline-flex items-center gap-1 text-[11px] text-warn">
                  <CircleAlert className="h-3.5 w-3.5" />
                  {t("xdebug.notLoaded")}
                </span>
              ) : (
                <span className="text-[11px] text-faint">{t("xdebug.notConfigured")}</span>
              )}
            </div>
            {/* 构建指纹：装错版本时的排查依据，直接摆出来 */}
            {st.build && (
              <p className="mt-1 font-mono text-[10.5px] text-faint">
                PHP {st.build.phpVersion} · {st.build.ts ? "TS" : "NTS"} · {st.build.compiler} ·{" "}
                {st.build.arch} · xdebug {st.recommended}
              </p>
            )}
            {!st.build && <p className="mt-1 text-[11px] text-warn">{t("xdebug.noBuild")}</p>}
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {busy && <Loader2 className="h-4 w-4 animate-spin text-primary" />}
            <Switch
              checked={st.enabled}
              disabled={busy}
              onCheckedChange={(v) => void toggle(v)}
              aria-label="Xdebug"
            />
          </div>
        </div>

        {/* 已启用但没加载成功：给出「重新配置」而不是让用户干瞪眼 */}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button
            size="sm"
            variant="secondary"
            className="h-7 text-[11.5px]"
            disabled={busy}
            onClick={() => void setup()}
          >
            <Sparkles className="h-3.5 w-3.5" />
            <span className="ml-1.5">{st.dllPresent ? t("xdebug.reconfigure") : t("xdebug.autosetup")}</span>
          </Button>
          {st.dllPresent && !st.loaded && (
            <Button
              size="sm"
              variant="ghost"
              className="h-7 text-[11.5px]"
              onClick={() =>
                void navigator.clipboard
                  .writeText(st.manualHint)
                  .then(() => toast.success(t("xdebug.hintCopied")))
              }
            >
              <FolderOpen className="h-3.5 w-3.5" />
              <span className="ml-1.5">{t("xdebug.copyHint")}</span>
            </Button>
          )}
        </div>

        {!st.dllPresent && st.manualHint && (
          <p className="mt-2 rounded-lg bg-card-2/50 p-2 font-mono text-[10.5px] leading-relaxed text-faint">
            {st.manualHint}
          </p>
        )}
      </div>
    </div>
  );
}

function ExtRow({
  ext,
  busy,
  onToggle,
}: {
  ext: PhpExtension;
  busy: boolean;
  onToggle: (next: boolean) => void;
}) {
  const t = useT();
  const broken = ext.missingDeps.length > 0;
  return (
    <div
      className={cn(
        "group flex items-center gap-3 rounded-lg border px-3 py-2 transition-colors",
        ext.enabled
          ? "border-border/70 bg-card-2/40"
          : "border-border/50 bg-transparent hover:border-border"
      )}
    >
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-[12.5px] font-medium">{ext.label}</span>
          {ext.label !== ext.name && (
            <span className="shrink-0 font-mono text-[10.5px] text-faint">{ext.name}</span>
          )}
          {ext.zend && (
            <Badge variant="outline" className="shrink-0 text-[9.5px]">
              Zend
            </Badge>
          )}
          {ext.builtin && (
            <Badge variant="outline" className="shrink-0 text-[9.5px] text-faint">
              {t("phpext.builtin")}
            </Badge>
          )}
          {broken && (
            <span title={t("phpext.depsNeeds")}>
              <ShieldAlert className="h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
            </span>
          )}
        </div>
        <p className="truncate text-[11px] text-faint">{ext.hint}</p>
      </div>
      {busy ? (
        <Loader2 className="h-4 w-4 shrink-0 animate-spin text-primary" />
      ) : (
        <Switch checked={ext.enabled} onCheckedChange={onToggle} aria-label={ext.label} />
      )}
    </div>
  );
}

/**
 * 「常用扩展一键补齐」——ServBay 里最省事的一步。
 * 补齐后端的真实逻辑在 Rust 侧，这里只负责触发与回报。
 */
export function PhpQuickSetupButton({
  version,
  onDone,
}: {
  version: string;
  onDone?: () => void;
}) {
  const t = useT();
  const [busy, setBusy] = React.useState(false);

  const run = async () => {
    setBusy(true);
    try {
      const v = await api.phpExtensions(version);
      // 只补「PHP 自带但没开」的常见扩展，不碰用户额外下载的
      const wanted = ["curl", "fileinfo", "gd", "mbstring", "mysqli", "openssl", "pdo_mysql", "sockets", "zip", "intl"];
      const todo = v.extensions.filter((e) => wanted.includes(e.name) && !e.enabled);
      if (todo.length === 0) {
        toast.info(t("phpext.alreadyComplete"));
        return;
      }
      const failed: string[] = [];
      for (const e of todo) {
        try {
          const r = await api.setPhpExtension(version, e.name, true);
          if (r.warnings.length > 0) failed.push(e.name);
        } catch {
          failed.push(e.name);
        }
      }
      const ok = todo.length - failed.length;
      if (failed.length === 0) {
        toast.success(t("phpext.quickDone").replace("{n}", String(ok)));
      } else {
        toast.warning(
          t("phpext.quickPartial").replace("{ok}", String(ok)).replace("{fail}", String(failed.length)),
          { description: failed.join(", "), duration: 9000 }
        );
      }
      onDone?.();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Button size="sm" variant="secondary" className="h-8 text-[12px]" onClick={() => void run()} disabled={busy}>
      {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Sparkles className="h-3.5 w-3.5" />}
      <span className="ml-1.5">{t("phpext.quickSetup")}</span>
    </Button>
  );
}

/** 供外部展示「已启用 N 个」的小徽标 */
export function PhpExtBadge({ version, onOpen }: { version: string; onOpen: () => void }) {
  const t = useT();
  const [count, setCount] = React.useState<number | null>(null);
  React.useEffect(() => {
    let alive = true;
    void api
      .phpExtensions(version)
      .then((v) => {
        if (alive) setCount(v.extensions.filter((e) => e.enabled).length);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [version]);
  return (
    <button
      type="button"
      onClick={onOpen}
      className="inline-flex items-center gap-1 rounded-md border border-border/70 bg-card-2/50 px-1.5 py-0.5 text-[10.5px] text-muted transition-colors hover:border-border-strong hover:text-foreground"
    >
      <Puzzle className="h-3 w-3" />
      {count == null ? "…" : t("phpext.badge").replace("{n}", String(count))}
    </button>
  );
}

/** 成功动画用的勾（保留给后续「补齐完成」反馈） */
export function PhpExtDoneCheck() {
  return (
    <motion.span
      initial={{ scale: 0, opacity: 0 }}
      animate={{ scale: 1, opacity: 1 }}
      transition={{ duration: 0.2 }}
    >
      <Check className="h-4 w-4 text-running" strokeWidth={3} />
    </motion.span>
  );
}
