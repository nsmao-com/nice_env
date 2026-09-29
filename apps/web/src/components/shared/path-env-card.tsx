"use client";

import * as React from "react";
import { useIsMutating, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Terminal, RefreshCw, CheckCircle2, AlertTriangle, FolderX } from "lucide-react";
import type { PathEnvEntry, PathEnvStatus } from "@nsb/schema";
import { useT } from "@/lib/store";
import { usePathEnv, toastError } from "@/lib/hooks";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";

/**
 * 环境变量（PATH 注入）。
 *
 * 和「终端注入」的区别：那个只是打印一段脚本让你粘到当前终端（关掉就没了），
 * 这个是真的写进系统 PATH（Windows 注册表 / macOS ~/.zshrc 托管块），
 * 新开的终端里 `php -v`、`mysql --version` 直接就能用。
 *
 * 界面刻意把「到底动了什么」摊开给用户看：每个已安装版本一行，显示注入的目录、
 * 会暴露哪些命令、是否已在 PATH 里 —— PATH 是很敏感的系统设置，
 * 用户必须能一眼确认我们加了什么，而不是只给一个开关。
 */
export function PathEnvCard() {
  const t = useT();
  const query = usePathEnv();
  const { data } = query;
  const ready = !!data && !query.error;
  const readError = query.error ? normalizeError(query.error) : null;
  const queryClient = useQueryClient();
  const [busy, setBusy] = React.useState(false);
  const [changeError, setChangeError] = React.useState<AppErrorShape | null>(null);
  const errorRef = React.useRef<HTMLDivElement>(null);
  const busyRef = React.useRef(false);
  const pathBusy = useIsMutating({ mutationKey: ["pathenv-change"] }) > 0;
  const mutation = useMutation({
    mutationKey: ["pathenv-change"],
    mutationFn: (fn: () => Promise<PathEnvStatus>) => fn(),
    onSuccess: (result) => { queryClient.setQueryData(["pathenv"], result); },
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["pathenv"] }),
  });
  React.useEffect(() => { if (changeError) errorRef.current?.focus(); }, [changeError]);

  const run = async (fn: () => Promise<PathEnvStatus>, okMsg: string) => {
    if (!ready || busyRef.current || queryClient.isMutating({ mutationKey: ["pathenv-change"] })) return;
    busyRef.current = true;
    setBusy(true); setChangeError(null);
    try {
      await mutation.mutateAsync(fn);
      toast.success(okMsg);
    } catch (e) {
      setChangeError(normalizeError(e));
      toastError(e);
    } finally {
      setBusy(false);
      busyRef.current = false;
    }
  };

  const entries = data?.entries ?? [];
  const enabled = data?.enabled ?? false;
  const toggleEntry = (entry: PathEnvEntry, next: boolean) => {
    run(() => api.pathenvSetVersion(entry.id, entry.version, next), `${entry.label} ${entry.version} · ${t("tools.pathEnv")} ✓`);
  };

  return (
    <Card>
      <CardHeader className="flex-row flex-wrap items-center gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
          <Terminal className="h-4 w-4 text-primary" strokeWidth={1.8} />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{t("tools.pathEnv")}</CardTitle>
          <CardDescription className="text-[11px]">{t("tools.pathEnvHint")}</CardDescription>
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Badge variant={ready && enabled ? "running" : "muted"} className="text-[10px]">
            {t(ready ? enabled ? "tools.pathEnvOn" : "tools.pathEnvOff" : readError ? "state.unknown" : "common.loading")}
          </Badge>
          {ready && <Switch
            checked={enabled}
            aria-label={t("tools.pathEnvEnable")}
            disabled={busy || pathBusy || !ready}
            onCheckedChange={(v) =>
              run(
                () => api.pathenvSetEnabled(v),
                v ? t("tools.pathEnvOn") : t("tools.pathEnvOff")
              )
            }
          />}
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {changeError && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-2 rounded-lg border border-error/25 bg-error-soft px-3 py-2.5 text-xs text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]">
          <p className="font-medium">{t("tools.pathEnvFailed")}</p>
          <p>{changeError.message}</p>
          {changeError.hint && <p>{changeError.hint}</p>}
          <p>{t("tools.pathEnvCheckAfterFailure")}</p>
          {changeError.detail && <details><summary className="cursor-pointer">{t("wz.errorDetails")}</summary><p className="mt-2 whitespace-pre-wrap">{changeError.detail}</p></details>}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="secondary" disabled={busy || pathBusy || query.isFetching} onClick={() => void query.refetch({ cancelRefetch: false })}>
              <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin motion-reduce:animate-none")} />{t("tools.pathEnvRetry")}
            </Button>
            {changeError.code === "PATH_UPDATE_FAILED" && <Button size="sm" variant="secondary" disabled={busy || pathBusy || !ready}
              onClick={() => run(() => api.pathenvReapply(), t("tools.pathEnvReapply"))}>{t("tools.pathEnvReapply")}</Button>}
          </div>
        </div>}
        {readError && <div role="alert" className="space-y-2 rounded-lg border border-error/25 bg-error-soft px-3 py-2.5 text-xs text-error [overflow-wrap:anywhere]">
          <p>{t("tools.pathEnvReadFailed")}</p>
          <p>{readError.message}</p>
          {readError.hint && <p>{readError.hint}</p>}
          <Button size="sm" variant="secondary" disabled={busy || pathBusy || query.isFetching} onClick={() => void query.refetch({ cancelRefetch: false })}>
            <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin motion-reduce:animate-none")} />{t("tools.pathEnvRetry")}
          </Button>
        </div>}
        {query.isPending && !readError && <p role="status" className="py-4 text-center text-[12px] text-faint">{t("common.loading")}</p>}

        {ready && entries.length === 0 && (
          <p className="py-4 text-center text-[12px] text-faint">{t("tools.pathEnvEmpty")}</p>
        )}

        {ready && entries.length > 0 && (
          <div className="flex flex-col gap-1.5">
            {entries.map((e) => (
              <PathEnvRow
                key={`${e.id}@${e.version}`}
                entry={e}
                disabled={busy || pathBusy || !ready || !enabled}
                onToggle={(next) => toggleEntry(e, next)}
              />
            ))}
          </div>
        )}

        {/* 漂移提示：PATH 被外部改过时给一键修复 */}
        {ready && data?.drift && (
          <div className="flex flex-wrap items-center gap-2 rounded-lg border border-warn/30 bg-warn/10 px-2.5 py-2">
            <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-warn" />
            <span className="min-w-0 flex-1 text-[11px] text-secondary [overflow-wrap:anywhere]">{t("tools.pathEnvDrift")}</span>
            <Button
              size="sm"
              variant="outline"
              disabled={busy || pathBusy || !ready}
              onClick={() => run(() => api.pathenvReapply(), t("tools.pathEnvReapply"))}
            >
              <RefreshCw className={cn("h-3 w-3", busy && "animate-spin motion-reduce:animate-none")} />
              {t("tools.pathEnvReapply")}
            </Button>
          </div>
        )}

        {/* 说明：为什么新终端才生效、以及不碰系统 PATH 的边界 */}
        {ready && data?.note && (
          <p className="text-[10.5px] leading-relaxed text-faint">{data.note}</p>
        )}
        <p className="text-[10.5px] leading-relaxed text-faint">
          {t("tools.pathEnvOnlyActive")} · {t("tools.pathEnvNoAdmin")}
        </p>
      </CardContent>
    </Card>
  );
}

function PathEnvRow({
  entry,
  disabled,
  onToggle,
}: {
  entry: PathEnvEntry;
  disabled: boolean;
  onToggle: (next: boolean) => void;
}) {
  const t = useT();
  return (
    <div
      className={cn(
        "flex items-center gap-2.5 rounded-lg border px-2.5 py-2 transition-colors",
        entry.selected && entry.inPath
          ? "border-border bg-card-2/40"
          : "border-border/60 bg-transparent"
      )}
    >
      <Switch
        checked={entry.selected}
        disabled={disabled}
        aria-label={`${entry.label} ${entry.version} PATH`}
        onCheckedChange={onToggle}
        title={entry.selected ? t("tools.pathEnvUncheck") : t("tools.pathEnvCheck")}
      />

      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1 [overflow-wrap:anywhere]">
          <span className="min-w-0 text-[12.5px] font-medium">{entry.label}</span>
          <span className="min-w-0 text-[10.5px] tabular text-faint">{entry.version}</span>
          {entry.inPath && (
            <CheckCircle2 className="h-3 w-3 shrink-0 text-running" aria-label={t("tools.pathEnvInPath")} />
          )}
          {!entry.exists && (
            <span className="inline-flex shrink-0 items-center gap-0.5 text-[10px] text-error">
              <FolderX className="h-3 w-3" />
              {t("tools.pathEnvMissing")}
            </span>
          )}
        </div>
        {/* 会暴露的命令：让用户知道注入后能敲什么 */}
        {entry.commands.length > 0 && (
          <div className="mt-1 flex flex-wrap items-center gap-1">
            {entry.commands.slice(0, 4).map((c) => (
              <code
                key={c}
                className="rounded bg-card-2/70 px-1.5 py-px font-mono text-[10.5px] text-secondary"
              >
                {c}
              </code>
            ))}
            {entry.commands.length > 4 && (
              <span className="text-[10px] text-faint">+{entry.commands.length - 4}</span>
            )}
          </div>
        )}
      </div>

      {/* 注入的目录：等宽 + 省略号，鼠标悬停看全路径 */}
      <span
        className="hidden max-w-[190px] shrink-0 truncate font-mono text-[10px] text-faint lg:block"
        title={entry.binDir}
      >
        {entry.binDir}
      </span>
    </div>
  );
}
