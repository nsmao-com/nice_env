"use client";

import * as React from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Loader2, Terminal } from "lucide-react";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { usePathEnv, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

/**
 * 套件行内的一键「加入 / 移出环境变量」。
 *
 * 和工具箱的 PathEnvCard 同源（pathenv_status），但把操作带到用户面前：
 * 装完一个运行时，就地开一下就写进系统 PATH，不用再绕去别的页面找入口。
 * 总开关没开时只选择当前包再开启，避免把其它包一并加入 PATH。
 */
export function PathEnvToggle({ pkgId, disabled = false }: { pkgId: string; disabled?: boolean }) {
  const t = useT();
  const { data, isLoading } = usePathEnv();
  const queryClient = useQueryClient();
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);

  const entry = data?.entries.find((e) => e.id === pkgId);
  const selected = entry?.selected ?? false;
  const enabled = data?.enabled ?? false;
  const inPath = enabled && selected && !!entry?.inPath;

  /** 后端没列出这个包（纯数据包 / 无可执行命令）：不显示，避免无效开关 */
  if (!isLoading && !entry) return null;

  const toggle = async () => {
    if (disabled || busyRef.current || !entry) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const ids = enabled ? (data?.entries ?? []).filter((e) => e.selected).map((e) => e.id) : [];
      const next = inPath
        ? ids.filter((x) => x !== pkgId)
        : Array.from(new Set([...ids, pkgId]));
      let result = await api.pathenvSetSelected(next);
      if (!enabled) result = await api.pathenvSetEnabled(true);
      queryClient.setQueryData(["pathenv"], result);
      toast.success(inPath ? t("tools.pathEnvRemovedToast") : t("tools.pathEnvAddedToast"));
    } catch (e) {
      toastError(e, t("tools.pathEnvFailed"));
    } finally {
      await queryClient.invalidateQueries({ queryKey: ["pathenv"] });
      busyRef.current = false;
      setBusy(false);
    }
  };

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          disabled={disabled || busy || isLoading}
          aria-label={`${t(inPath ? "tools.pathEnvRemove" : "tools.pathEnvAdd")} ${pkgId} ${entry?.version ?? ""}`}
          aria-pressed={inPath}
          onClick={toggle}
          className={cn(
            "inline-flex shrink-0 items-center gap-1 rounded-md border px-1.5 py-0.5 text-[10.5px] transition-colors disabled:opacity-50",
            inPath
              ? "border-primary/40 bg-primary-soft text-primary"
              : "border-border/70 bg-card-2/50 text-muted hover:border-border-strong hover:text-foreground"
          )}
        >
          {busy ? <Loader2 className="h-3 w-3 animate-spin" /> : <Terminal className="h-3 w-3" />}
          PATH
        </button>
      </TooltipTrigger>
      <TooltipContent side="top" className="max-w-64">
        <span className="font-medium">{t(inPath ? "tools.pathEnvRemove" : "tools.pathEnvAdd")} {entry?.version}</span>
        {entry && entry.commands.length > 0 && (
          <span className="block text-[10.5px] opacity-70">
            {t("tools.pathEnvCommands")}：{entry.commands.slice(0, 4).join(" · ")}
          </span>
        )}
      </TooltipContent>
    </Tooltip>
  );
}
