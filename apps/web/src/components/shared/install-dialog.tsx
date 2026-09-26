"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  AlertTriangle,
  ArrowRight,
  Check,
  Download,
  FileArchive,
  HardDrive,
  Loader2,
  Package,
  Settings2,
  ShieldCheck,
  X,
} from "lucide-react";
import type { PackageView, ServiceStatus } from "@nsb/schema";
import { cn, fmtBytes, fmtSpeed, fmtDuration } from "@/lib/utils";
import { useT } from "@/lib/store";
import { toastError, useInvalidate } from "@/lib/hooks";
import { useInstallTasks } from "@/lib/install-tasks";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogTitle,
} from "@/components/ui/dialog";
import { RingProgress } from "@/components/shared/ring-progress";
import { PathEnvToggle } from "@/components/shared/path-env-toggle";

/* ============================================================
   套件安装向导：把「下载 → 校验 → 解压 → 写配置 → 完成」
   这条链路可视化。之前只有一个 toast「安装中…」，用户看不到
   进度、也分不清卡在哪一步；这里用后端真实事件驱动阶段时间线。
   ============================================================ */

const STAGES = [
  { id: "download", icon: Download, labelKey: "install.stage.download" },
  { id: "verify", icon: ShieldCheck, labelKey: "install.stage.verify" },
  { id: "extract", icon: FileArchive, labelKey: "install.stage.extract" },
  { id: "config", icon: Settings2, labelKey: "install.stage.config" },
] as const;

type StageId = (typeof STAGES)[number]["id"] | "done";

/** 后端 InstallState → 时间线阶段（同一个阶段内的细节状态归到主阶段） */
function stageFromState(state: string): StageId {
  switch (state) {
    case "downloading":
      return "download";
    case "downloaded":
    case "verifying":
      return "verify";
    case "extracting":
      return "extract";
    case "configuring":
      return "config";
    case "installed":
      return "done";
    default:
      return "download";
  }
}

export interface InstallTarget {
  id: string;
  displayName: string;
  version: string;
  sizeBytes?: number;
  /** 已装时二次安装 = 重装提示 */
  reinstall?: boolean;
}

export function InstallDialog({
  target,
  onOpenChange,
  onDone,
  /** 完成后是否询问启动（服务型包） */
  startableAs,
}: {
  target: InstallTarget | null;
  onOpenChange: (open: boolean) => void;
  onDone?: () => void;
  /** 传服务 id 表示装完可直接启动 */
  startableAs?: string | null;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const [starting, setStarting] = React.useState(false);
  const startedRef = React.useRef<string | null>(null);

  const taskId = target ? `${target.id}@${target.version}` : null;
  /* 安装本身是全局后台任务（见 lib/install-tasks），弹窗只负责展示，随时可关 */
  const task = useInstallTasks((s) => (taskId ? s.tasks[taskId] : undefined));
  const progress = useInstallTasks((s) => (taskId ? s.progress[taskId] : undefined)) ?? null;
  const startTask = useInstallTasks((s) => s.start);
  const cancelTask = useInstallTasks((s) => s.cancel);

  const busy = task?.status === "running";
  const finished = task?.status === "done";
  const cancelled = task?.status === "cancelled";
  const cancelling = busy && !!task?.cancelRequested;
  const error = task?.status === "error" ? (task.error ?? t("install.failed")) : null;
  const stage: StageId = finished ? "done" : progress ? stageFromState(progress.state) : "download";

  /* 打开即开始安装；同一版本已在后台安装时只是重新显示它的进度 */
  React.useEffect(() => {
    if (!target) {
      startedRef.current = null;
      return;
    }
    if (startedRef.current === taskId) return;
    startedRef.current = taskId;
    void startTask(target);
  }, [target, taskId, startTask]);

  /* 页面级的完成回调（全局刷新由 InstallTasksBridge 负责，这里只在弹窗还开着时补调） */
  const onDoneRef = React.useRef(onDone);
  onDoneRef.current = onDone;
  const prevStatusRef = React.useRef(task?.status);
  React.useEffect(() => {
    if (task?.status === "done" && prevStatusRef.current === "running") onDoneRef.current?.();
    prevStatusRef.current = task?.status;
  }, [task?.status]);

  const retry = () => {
    if (target) void startTask(target);
  };

  const cancel = async () => {
    if (!taskId) return;
    await cancelTask(taskId);
  };

  /* 关闭弹窗不影响安装：进行中关掉就转入后台，装完会有通知 */
  const close = (open: boolean) => {
    if (!open && starting) return;
    if (!open && busy) toast.info(t("install.background"));
    onOpenChange(open);
  };

  const startNow = async () => {
    if (!startableAs || starting) return;
    setStarting(true);
    try {
      if (target && startableAs === target.id) {
        await api.setActiveVersion(target.id, target.version);
      }
      await api.startService(startableAs);
      toast.success(t("common.running"));
      onOpenChange(false);
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(false);
      invalidate("services", "packages", "pathenv");
    }
  };

  const stageIndex = STAGES.findIndex((s) => s.id === stage);
  const pct = progress && progress.total > 0 ? Math.min(100, (progress.received / progress.total) * 100) : 0;

  return (
    <Dialog open={target !== null} onOpenChange={close}>
      <DialogContent hideClose={starting} className="flex max-h-[calc(100dvh-24px)] max-w-lg flex-col gap-0 overflow-hidden p-0">
        {/* 头部 */}
        <div className="relative shrink-0 border-b border-border bg-card-2/30 py-5 pl-4 pr-12 sm:pl-6">
          <div className="absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-primary/40 to-transparent" />
          <div className="flex items-start gap-4">
            <div
              className={cn(
                "flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl border",
                finished ? "border-running/30 bg-running-soft" : "border-primary/30 bg-primary-soft"
              )}
            >
              {finished ? (
                <Check className="h-5 w-5 text-running" />
              ) : error ? (
                <AlertTriangle className="h-5 w-5 text-error" />
              ) : (
                <Package className="h-5 w-5 text-primary" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <DialogTitle className="text-[15px] leading-snug [overflow-wrap:anywhere]">
                {cancelled ? t("install.cancelled") : error
                  ? t("install.failed")
                  : finished
                    ? t("install.stage.done")
                    : `${t("install.title")} · ${target?.displayName ?? ""}`}
              </DialogTitle>
              <DialogDescription className="mt-1 flex flex-wrap items-center gap-x-2 text-[12px]">
                <span className="font-mono [overflow-wrap:anywhere]">v{target?.version}</span>
                {target?.sizeBytes ? (
                  <>
                    <span className="text-faint">·</span>
                    <span>{fmtBytes(target.sizeBytes)}</span>
                  </>
                ) : null}
                {!finished && !error && !cancelled && (
                  <>
                    <span className="text-faint">·</span>
                    <span className="text-faint">{t("install.keepOpen")}</span>
                  </>
                )}
              </DialogDescription>
            </div>
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">
          {/* 阶段时间线 */}
          <div className="mb-5 flex items-center gap-1.5">
            {STAGES.map((s, i) => {
              const done = finished || i < stageIndex;
              const active = !finished && !cancelled && i === stageIndex && !error;
              return (
                <React.Fragment key={s.id}>
                  <div className="flex min-w-0 flex-1 flex-col items-center gap-1.5">
                    <div
                      className={cn(
                        "flex h-8 w-8 items-center justify-center rounded-full border transition-colors",
                        done
                          ? "border-running/40 bg-running-soft text-running"
                          : active
                            ? "border-primary/50 bg-primary-soft text-primary"
                            : "border-border bg-card-2/40 text-faint"
                      )}
                    >
                      {done ? (
                        <Check className="h-3.5 w-3.5" />
                      ) : active ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <s.icon className="h-3.5 w-3.5" />
                      )}
                    </div>
                    <span
                      className={cn(
                        "min-h-[2.5em] max-w-full text-center text-[10px] leading-tight text-balance",
                        done || active ? "text-secondary" : "text-faint"
                      )}
                    >
                      {t(s.labelKey)}
                    </span>
                  </div>
                  {i < STAGES.length - 1 && (
                    <div className="mb-8 h-px w-3 shrink-0 bg-border">
                      <motion.div
                        className="h-full bg-running/60"
                        initial={false}
                        animate={{ width: done ? "100%" : "0%" }}
                        transition={{ duration: 0.3 }}
                      />
                    </div>
                  )}
                </React.Fragment>
              );
            })}
          </div>

          {/* 下载进度 */}
          <AnimatePresence mode="wait">
            {cancelled ? (
              <motion.div key="cancelled" initial={{ opacity: 0 }} animate={{ opacity: 1 }}
                className="rounded-xl border border-border bg-fill p-3.5 text-[12px] text-secondary" role="status">
                {t("install.cancelledHint")}
              </motion.div>
            ) : error ? (
              <motion.div
                key="error"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                className="flex flex-col gap-2 rounded-xl border border-error/30 bg-error/10 p-3.5"
                role="alert"
              >
                <div className="flex items-center gap-2 text-[12.5px] font-medium text-error">
                  <AlertTriangle className="h-3.5 w-3.5" /> {t("install.failed")}
                </div>
                <p className="whitespace-pre-wrap text-[11.5px] leading-relaxed text-secondary [overflow-wrap:anywhere]">{error}</p>
              </motion.div>
            ) : finished ? (
              <motion.div
                key="done"
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                className="flex items-center gap-3 rounded-xl border border-running/25 bg-running-soft/60 p-3.5"
              >
                <HardDrive className="h-4 w-4 shrink-0 text-running" />
                <div className="flex min-w-0 flex-col">
                  <span className="text-[12.5px] font-medium text-secondary">
                    {target?.displayName} {target?.version}
                  </span>
                  <span className="text-[11px] text-faint">{t("install.doneHint")}</span>
                </div>
                {/* 装完顺手把命令加进环境变量：就地一步，不再跑去找入口 */}
                {target && (
                  <div className="ml-auto shrink-0">
                    <PathEnvToggle pkgId={target.id} version={target.version} />
                  </div>
                )}
              </motion.div>
            ) : (
              <motion.div
                key="progress"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                className="flex items-center gap-4 rounded-xl border border-primary/25 bg-primary-soft/40 p-3.5"
              >
                <RingProgress
                  value={pct}
                  size={58}
                  strokeWidth={5}
                  indeterminate={stage !== "download" || (progress?.total ?? 0) === 0}
                >
                  <span className="text-[11px] font-semibold tabular text-primary">
                    {stage === "download" && progress && progress.total > 0 ? `${pct.toFixed(0)}%` : "…"}
                  </span>
                </RingProgress>
                <div className="flex min-w-0 flex-1 flex-col gap-1.5">
                  <span className="text-[12.5px] font-medium text-secondary" role="status">
                    {cancelling ? t("install.cancelling") : t(STAGES[Math.max(0, stageIndex)]?.labelKey ?? "install.stage.download")}
                  </span>
                  {stage === "download" && progress ? (
                    <>
                      <div className="h-1.5 overflow-hidden rounded-full bg-card-2">
                        <motion.div
                          className="h-full rounded-full bg-primary"
                          animate={{ width: `${pct}%` }}
                          transition={{ duration: 0.25 }}
                        />
                      </div>
                      <span className="tabular text-[10.5px] text-faint">
                        {fmtBytes(progress.received)}
                        {progress.total > 0 ? ` / ${fmtBytes(progress.total)}` : ""}
                        {progress.speedBps > 0 ? ` · ${fmtSpeed(progress.speedBps)}` : ""}
                        {progress.etaSec > 0 ? ` · ${t("packages.eta")} ${fmtDuration(progress.etaSec)}` : ""}
                      </span>
                    </>
                  ) : (
                    <span className="text-[10.5px] text-faint">{t("ob.bigFileHint")}</span>
                  )}
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        <DialogFooter className="shrink-0 border-t border-border bg-card-2/20 px-4 py-3.5 sm:px-6">
          {error || cancelled ? (
            <>
              <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={starting}>
                {t("common.close")}
              </Button>
              <Button onClick={retry} disabled={busy}>
                {t("install.retry")}
              </Button>
            </>
          ) : finished ? (
            <>
              <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={starting}>
                {t("install.close")}
              </Button>
              {startableAs && (
                <Button onClick={startNow} disabled={starting}>
                  {starting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <ArrowRight className="h-3.5 w-3.5" />} {t(starting ? "packages.starting" : "common.start")}
                </Button>
              )}
            </>
          ) : (
            <>
              <Button variant="ghost" onClick={cancel} disabled={cancelling || stage === "config"}>
                {cancelling ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <X className="h-3.5 w-3.5" />} {t(cancelling ? "install.cancelling" : "install.cancel")}
              </Button>
              <Button onClick={() => close(false)}>{t("install.runInBackground")}</Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 供套件页判断：这个包能不能在装完后直接启动 */
export function serviceIdFor(p: Pick<PackageView, "id" | "version" | "run">): string | null {
  if (!p.run) return null;
  return p.run.singleInstance === false ? `${p.id}@${p.version}` : p.id;
}

export type { ServiceStatus };
