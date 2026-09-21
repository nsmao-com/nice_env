"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  Package,
  Loader2,
  RotateCcw,
  Check,
  Info,
  FileCog,
} from "lucide-react";
import type { ToolMirrorStatus } from "@nsb/schema";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/shared/misc";
import { Skeleton } from "@/components/ui/misc";

/**
 * 工具链镜像源。
 *
 * 与「套件下载镜像」不是一回事：那个管本应用下载套件走哪条路；
 * 这个管**用户在项目里 `composer install` / `npm install` 时**走哪条路。
 * 国内直连官方源经常几十 KB/s，换镜像能快一个数量级。
 *
 * 因为它会改**全局配置文件**（~/.npmrc、Composer 的 config.json），
 * 属于会影响用户机器其它项目的操作，所以：
 * - 明确显示会改哪个文件
 * - 切换前二次确认
 * - 提供「恢复官方源」
 */
export function ToolMirrorCard() {
  const t = useT();
  const [list, setList] = React.useState<ToolMirrorStatus[] | null>(null);
  const [busy, setBusy] = React.useState<string | null>(null);
  const [confirm, setConfirm] = React.useState<{
    manager: string;
    url: string;
    label: string;
  } | null>(null);

  const load = React.useCallback(async () => {
    try {
      setList(await api.toolMirrors());
    } catch {
      setList([]);
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  const apply = async (manager: string, url: string, label: string) => {
    setBusy(manager);
    try {
      await api.toolMirrorSet(manager, url);
      toast.success(t("mirror.switched").replace("{name}", label));
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  const reset = async (manager: string) => {
    setBusy(manager);
    try {
      await api.toolMirrorReset(manager);
      toast.success(t("mirror.resetDone"));
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <p className="flex items-start gap-2 text-[12px] leading-relaxed text-muted">
        <Info className="mt-0.5 h-3.5 w-3.5 shrink-0 text-faint" />
        {t("mirror.hint")}
      </p>

      {list == null ? (
        <div className="space-y-2">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-16 w-full" />
          ))}
        </div>
      ) : (
        list.map((m) => {
          const isOfficial = m.matched === "official" || !m.current;
          return (
            <div
              key={m.manager}
              className="rounded-xl border border-border/70 bg-card-2/25 p-3"
            >
              <div className="flex flex-wrap items-center gap-2">
                <Package className="h-3.5 w-3.5 text-faint" />
                <span className="text-[12.5px] font-medium capitalize">{m.manager}</span>
                {m.current && (
                  <Badge
                    variant="outline"
                    className={cn("text-[9.5px]", isOfficial ? "" : "text-running")}
                  >
                    {m.matched
                      ? (m.options.find((o) => o.id === m.matched)?.label ?? m.matched)
                      : t("mirror.custom")}
                  </Badge>
                )}
                {!m.current && (
                  <span className="text-[10.5px] text-faint">{t("mirror.notSet")}</span>
                )}
                {busy === m.manager && (
                  <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
                )}
                {!isOfficial && (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="ml-auto h-6 px-1.5 text-[11px]"
                    disabled={busy != null}
                    onClick={() => void reset(m.manager)}
                  >
                    <RotateCcw className="h-3 w-3" />
                    <span className="ml-1">{t("mirror.reset")}</span>
                  </Button>
                )}
              </div>

              {m.current && (
                <p className="mt-1 truncate font-mono text-[10.5px] text-faint">{m.current}</p>
              )}

              {/* 会改哪个文件必须写清楚 —— 这是全局配置，会影响别的项目 */}
              {m.configPath && (
                <p className="mt-0.5 flex items-center gap-1 truncate font-mono text-[10px] text-faint/80">
                  <FileCog className="h-3 w-3 shrink-0" />
                  {m.configPath}
                </p>
              )}

              <div className="mt-2 flex flex-wrap gap-1.5">
                {m.options.map((o) => {
                  const active = m.matched === o.id;
                  return (
                    <button
                      key={o.id}
                      type="button"
                      disabled={busy != null}
                      onClick={() => {
                        if (active) return;
                        // 改全局配置前先确认：用户可能只想给某个项目提速，
                        // 不想动整台机器的源
                        setConfirm({
                          manager: m.manager,
                          url: o.url,
                          label: o.label,
                        });
                      }}
                      title={o.note}
                      className={cn(
                        "inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-[10.5px] transition-colors disabled:opacity-50",
                        active
                          ? "border-primary/40 bg-primary-soft text-primary"
                          : "border-border/70 bg-card-2/50 text-muted hover:border-border-strong hover:text-foreground"
                      )}
                    >
                      {active && <Check className="h-3 w-3" />}
                      {o.label}
                    </button>
                  );
                })}
              </div>
            </div>
          );
        })
      )}

      <ConfirmDialog
        open={confirm != null}
        onOpenChange={(v) => !v && setConfirm(null)}
        title={t("mirror.confirmTitle")}
        description={t("mirror.confirmDesc")
          .replace("{name}", confirm?.label ?? "")
          .replace("{url}", confirm?.url ?? "")}
        confirmText={t("mirror.confirmBtn")}
        onConfirm={() => {
          const c = confirm;
          setConfirm(null);
          if (c) void apply(c.manager, c.url, c.label);
        }}
      />
    </div>
  );
}
