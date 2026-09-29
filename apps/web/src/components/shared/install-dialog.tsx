"use client";

import * as React from "react";
import { useIsMutating, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  AlertTriangle,
  ArrowRight,
  Check,
  ChevronDown,
  Download,
  FileArchive,
  HardDrive,
  Loader2,
  Package,
  RefreshCw,
  Settings2,
  ShieldCheck,
  X,
} from "lucide-react";
import type { PackageView, ServiceStatus } from "@nsb/schema";
import { cn, fmtBytes, fmtSpeed, fmtDuration, sameVersion } from "@/lib/utils";
import { useT } from "@/lib/store";
import { serviceHasProcess, useInvalidate } from "@/lib/hooks";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
import { progressForTask, useInstallTasks, type InstallTask } from "@/lib/install-tasks";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
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
      // 下载器结束后后端还需完成注册；任务 Promise 成功才显示安装完成。
      return "config";
    default:
      return "download";
  }
}

export interface InstallTarget {
  id: string;
  displayName: string;
  version?: string;
  sizeBytes?: number;
  /** 查看已有任务不会再次发起安装；只有显式“重试”才会重新调用后端。 */
  inspect?: boolean;
  taskKey?: string;
  /** 已装时二次安装 = 重装提示 */
  reinstall?: boolean;
}

export function InstallDialog({
  target,
  onOpenChange,
  onCloseAutoFocus,
  onDone,
  /** 完成后是否询问启动（服务型包） */
  startableAs,
}: {
  target: InstallTarget | null;
  onOpenChange: (open: boolean) => void;
  onCloseAutoFocus?: React.ComponentProps<typeof DialogContent>["onCloseAutoFocus"];
  onDone?: () => void;
  /** 传服务 id 表示装完可直接启动 */
  startableAs?: string | null;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const queryClient = useQueryClient();
  const pathBusy = useIsMutating({ mutationKey: ["pathenv-change"] }) > 0;
  const [starting, setStarting] = React.useState(false);
  const [startError, setStartError] = React.useState<AppErrorShape | null>(null);
  const startErrorRef = React.useRef<HTMLDivElement>(null);
  const startRef = React.useRef<object | null>(null);
  const targetRef = React.useRef(target);
  targetRef.current = target;
  const startedRef = React.useRef<string | null>(null);

  const taskId = target ? target.taskKey ?? (target.version ? `${target.id}@${target.version}` : target.id) : null;
  /* 安装本身是全局后台任务（见 lib/install-tasks），弹窗只负责展示，随时可关 */
  const task = useInstallTasks((s) => (taskId ? s.tasks[taskId] : undefined));
  const progress = useInstallTasks((s) => progressForTask(s.progress, taskId ? s.tasks[taskId] : undefined)) ?? null;
  const startTask = useInstallTasks((s) => s.start);
  const cancelTask = useInstallTasks((s) => s.cancel);
  const displayVersion = task?.resolvedVersion ?? target?.version;
  const missing = !!target?.inspect && !task;

  const busy = task?.status === "running";
  const finished = task?.status === "done";
  const cancelled = task?.status === "cancelled";
  const cancelling = busy && !!task?.cancelRequested;
  const error = missing ? t("install.taskMissing") : task?.status === "error" ? (task.error ?? t("install.failed")) : null;
  const stage: StageId = finished ? "done" : progress ? stageFromState(progress.state) : "download";

  React.useEffect(() => {
    startRef.current = null;
    setStarting(false);
    setStartError(null);
    return () => { startRef.current = null; };
  }, [target, taskId]);

  React.useEffect(() => {
    if (startError) startErrorRef.current?.scrollIntoView({ block: "nearest" });
  }, [startError]);

  /* 打开即开始安装；同一版本已在后台安装时只是重新显示它的进度 */
  React.useEffect(() => {
    if (!target) {
      startedRef.current = null;
      return;
    }
    if (startedRef.current === taskId) return;
    startedRef.current = taskId;
    if (!target.inspect) void startTask(target);
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
    if (!open && startRef.current) return;
    if (!open && busy) toast.info(t("install.background"));
    onOpenChange(open);
  };

  const startNow = async () => {
    if (!target || !taskId || !finished || !displayVersion || !startableAs || startRef.current
      || queryClient.isMutating({ mutationKey: ["pathenv-change"] })) return;
    const operation = {};
    startRef.current = operation;
    const current = () => startRef.current === operation && targetRef.current === target;
    const sameCompletedTask = () => {
      const latest = useInstallTasks.getState().tasks[taskId];
      return latest === task && latest?.status === "done" && latest.id === target.id
        && sameVersion(latest.resolvedVersion ?? latest.version, displayVersion);
    };
    setStarting(true);
    setStartError(null);
    let syncedPathTasks: InstallTask[] = [];
    try {
      const [packages, services] = await Promise.all([
        queryClient.fetchQuery({ queryKey: ["packages"], queryFn: api.listPackages, staleTime: 0, retry: false, networkMode: "always" }),
        queryClient.fetchQuery({ queryKey: ["services"], queryFn: api.listServiceStatus, staleTime: 0, retry: false, networkMode: "always" }),
      ]);
      if (!current()) return;
      if (!sameCompletedTask()) throw { code: "INSTALL_TASK_CHANGED", message: t("install.startTaskChanged") };
      const installed = packages.find((pkg) => pkg.id === target.id && sameVersion(pkg.version, displayVersion) && pkg.install);
      if (!installed) throw { code: "NOT_INSTALLED", message: t("versions.noLongerInstalled") };
      if (serviceIdFor(installed) !== startableAs) throw { code: "SERVICE_TARGET_CHANGED", message: t("versions.serviceChanged") };
      const validateService = (service: ServiceStatus | undefined, allowSwitch = false) => {
        if (!service) throw { code: "UNKNOWN_SERVICE", message: t("svc.notFound") };
        if (service.state === "starting" || service.state === "stopping") {
          throw { code: "SERVICE_BUSY", message: t(service.state === "starting" ? "state.starting" : "state.stopping") };
        }
        if (service.state === "unknown") throw { code: "SERVICE_STATE_UNKNOWN", message: t("packages.statusUnknown") };
        if (!sameVersion(service.version, displayVersion)) {
          if (!allowSwitch) throw { code: "SERVICE_TARGET_CHANGED", message: t("versions.serviceChanged") };
          if (serviceHasProcess(service)) throw { code: "SERVICE_BUSY", message: t("packages.switchRunning") };
        } else if (service.state === "running") {
          return service;
        }
        if (serviceHasProcess(service)) throw { code: "SERVICE_BUSY", message: t("svc.processStillRunning") };
        if (service.missingRequires.length) {
          throw { code: "MISSING_DEPENDENCIES", message: t("svc.needDepsHint"), hint: service.missingRequires.join(" · ") };
        }
        return service;
      };
      const singleInstance = startableAs === target.id;
      let service = validateService(services.find((service) => service.id === startableAs), singleInstance);
      if (service.state !== "running" && singleInstance) {
        // PATH 同步失败后可以继续重试，即使默认版本选择已保存。
        const pendingPathTasks = Object.values(useInstallTasks.getState().tasks).filter((task) => task.status === "done" && task.pathSyncError);
        await api.setActiveVersion(target.id, displayVersion);
        syncedPathTasks = pendingPathTasks;
        if (!current()) return;
        const selected = await queryClient.fetchQuery({ queryKey: ["services"], queryFn: api.listServiceStatus, staleTime: 0, retry: false, networkMode: "always" });
        if (!current()) return;
        service = validateService(selected.find((service) => service.id === startableAs));
      }
      if (!sameCompletedTask()) throw { code: "INSTALL_TASK_CHANGED", message: t("install.startTaskChanged") };
      if (service.state !== "running") await api.startService(startableAs, service.version);
      if (!current() || !sameCompletedTask()) return;
      toast.success(`${target.displayName} ${displayVersion} · ${t("common.running")}`);
      onOpenChange(false);
    } catch (e) {
      if (current()) setStartError(normalizeError(e));
    } finally {
      if (current()) {
        startRef.current = null;
        setStarting(false);
      }
      for (const synced of syncedPathTasks) useInstallTasks.getState().updatePathSyncError(synced);
      invalidate("services", "packages", "pathenv", "stacks", "databases", "db-users");
    }
  };

  const stageIndex = STAGES.findIndex((s) => s.id === stage);
  const pct = progress && progress.total > 0 ? Math.min(100, (progress.received / progress.total) * 100) : 0;

  return (
    <Dialog open={target !== null} onOpenChange={close}>
      <DialogContent hideClose={starting} onCloseAutoFocus={onCloseAutoFocus} className="flex max-h-[calc(100dvh-24px)] max-w-lg flex-col gap-0 overflow-hidden p-0">
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
                <span className="font-mono [overflow-wrap:anywhere]">{displayVersion ? `v${displayVersion}` : t("install.automaticVersion")}</span>
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
                className="flex flex-wrap items-center gap-3 rounded-xl border border-running/25 bg-running-soft/60 p-3.5"
              >
                <HardDrive className="h-4 w-4 shrink-0 text-running" />
                <div className="flex min-w-0 flex-1 basis-36 flex-col [overflow-wrap:anywhere]">
                  <span className="text-[12.5px] font-medium text-secondary">
                    {target?.displayName} {displayVersion}
                  </span>
                  <span className="text-[11px] text-faint">{t("install.doneHint")}</span>
                </div>
                {/* 装完顺手把命令加进环境变量：就地一步，不再跑去找入口 */}
                {target && displayVersion && (
                  <div className="ml-auto shrink-0">
                    <PathEnvToggle pkgId={target.id} version={displayVersion} disabled={starting} />
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
          {finished && task?.pathSyncError && <InstallPathWarning key={`${task.key}:${task.startedAt}`} task={task} disabled={starting} />}
          {finished && <p className="mt-3 text-[11.5px] leading-relaxed text-muted [overflow-wrap:anywhere]">{t("install.defaultPreservedHint")}</p>}
          {finished && startableAs === target?.id && <p className="mt-3 text-[11.5px] leading-relaxed text-muted">{t("install.startVersionHint")}</p>}
          {finished && startError && <div ref={startErrorRef} role="alert" className="mt-3 rounded-xl border border-error/30 bg-error-soft p-3 text-[12px] text-error [overflow-wrap:anywhere]">
            <p className="font-medium">{t("install.startFailed")}</p>
            <p className="mt-1 whitespace-pre-wrap">{startError.message}</p>
            {startError.hint && <p className="mt-1 whitespace-pre-wrap">{startError.hint}</p>}
          </div>}
        </div>

        <DialogFooter className="shrink-0 border-t border-border bg-card-2/20 px-4 py-3.5 sm:px-6">
          {error || cancelled ? (
            <>
              <Button variant="ghost" onClick={() => close(false)} disabled={starting}>
                {t("common.close")}
              </Button>
              {!missing && <Button onClick={retry} disabled={busy}>
                {t("install.retry")}
              </Button>}
            </>
          ) : finished ? (
            <>
              <Button variant="ghost" onClick={() => close(false)} disabled={starting}>
                {t("install.close")}
              </Button>
              {startableAs && (
                <Button onClick={startNow} disabled={starting || pathBusy || !displayVersion}>
                  {starting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <ArrowRight className="h-3.5 w-3.5" />} {t(starting ? "packages.starting" : startError ? "install.retryStart" : "common.start")}
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

/** 安装已成功，单独恢复环境变量；关闭详情也保留任务中的待处理提示。 */
function InstallPathWarning({ task, disabled }: { task: InstallTask; disabled: boolean }) {
  const t = useT();
  const queryClient = useQueryClient();
  const pathBusy = useIsMutating({ mutationKey: ["pathenv-change"] }) > 0;
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const mountedRef = React.useRef(true);
  React.useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  const mutation = useMutation({
    mutationKey: ["pathenv-change"],
    mutationFn: api.pathenvReapply,
    onSuccess: (result) => queryClient.setQueryData(["pathenv"], result),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["pathenv"] }),
  });
  const retry = async () => {
    if (disabled || busyRef.current || queryClient.isMutating({ mutationKey: ["pathenv-change"] })
      || useInstallTasks.getState().tasks[task.key] !== task) return;
    busyRef.current = true;
    setBusy(true);
    const pendingPathTasks = Object.values(useInstallTasks.getState().tasks).filter((task) => task.status === "done" && task.pathSyncError);
    try {
      const result = await mutation.mutateAsync();
      if (result.drift) throw { code: "PATH_SYNC_UNCONFIRMED", message: t("install.pathSyncUnconfirmed") };
      // 重新应用会同步所有当前选择，只清理开始重试前已有且未变化的提醒。
      for (const synced of pendingPathTasks) useInstallTasks.getState().updatePathSyncError(synced);
      toast.success(t("install.pathSyncDone"));
    } catch (error) {
      useInstallTasks.getState().updatePathSyncError(task, normalizeError(error));
    } finally {
      busyRef.current = false;
      if (mountedRef.current) setBusy(false);
    }
  };
  return <div role="alert" className="mt-3 space-y-2 rounded-xl border border-warn/30 bg-warn-soft p-3 text-[12px] [overflow-wrap:anywhere]">
    <p className="flex items-center gap-2 font-medium text-warn"><AlertTriangle className="h-4 w-4 shrink-0" />{t("install.pathSyncPending")}</p>
    <p className="leading-relaxed text-secondary">{t("install.pathSyncHint")}</p>
    <p className="whitespace-pre-wrap text-muted">{task.pathSyncError?.message}</p>
    {task.pathSyncError?.hint && <p className="whitespace-pre-wrap text-muted">{task.pathSyncError.hint}</p>}
    <Button size="sm" variant="outline" disabled={disabled || busy || pathBusy} onClick={() => void retry()}>
      <RefreshCw className={cn("h-3.5 w-3.5", busy && "animate-spin motion-reduce:animate-none")} />{t("install.retryPathSync")}
    </Button>
  </div>;
}

/** 会话内的任务入口独立于套件筛选；切换页面或关闭进度弹窗后仍可查看结果。 */
export function InstallTasksPanel({ onInspect }: {
  onInspect: (target: InstallTarget, trigger: HTMLButtonElement | null) => void;
}) {
  const t = useT();
  const tasks = useInstallTasks((s) => s.tasks);
  const dismiss = useInstallTasks((s) => s.dismiss);
  const [expanded, setExpanded] = React.useState(true);
  const [hasShown, setHasShown] = React.useState(false);
  const contentId = React.useId();
  const toggleRef = React.useRef<HTMLButtonElement>(null);
  const rank = { running: 0, error: 1, cancelled: 2, done: 3 };
  const taskRank = (task: InstallTask) => task.status === "done" && task.pathSyncError ? 1 : rank[task.status];
  const list = Object.values(tasks).sort((a, b) => taskRank(a) - taskRank(b) || b.startedAt - a.startedAt);
  React.useEffect(() => {
    if (list.length) setHasShown(true);
  }, [list.length]);
  if (!list.length && !hasShown) return null;
  const running = list.filter((task) => task.status === "running").length;
  const failed = list.filter((task) => task.status === "error").length;
  const pendingPath = list.filter((task) => task.status === "done" && task.pathSyncError).length;
  return <Card role="region" className="mb-4 overflow-hidden" aria-label={t("install.tasks")}>
    <div className="flex flex-wrap items-center gap-2 p-3">
      <button ref={toggleRef} type="button" aria-expanded={expanded} aria-controls={contentId} onClick={() => setExpanded(!expanded)}
        className="flex min-h-9 min-w-0 flex-1 basis-full items-center gap-2 rounded-lg px-1 text-left text-sm font-medium focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary sm:basis-0">
        <Download className="h-4 w-4 shrink-0" />
        <span className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1">
          <span>{t("install.tasks")}</span><span className="text-muted">{list.length}</span>
          {running > 0 && <span className="text-xs text-info">{running} {t("install.tasksRunning")}</span>}
          {failed > 0 && <span className="text-xs text-error">{failed} {t("install.failed")}</span>}
          {pendingPath > 0 && <span className="text-xs text-warn">{pendingPath} {t("install.pathSyncCount")}</span>}
        </span>
        <ChevronDown className={cn("h-4 w-4 shrink-0", expanded && "rotate-180")} />
      </button>
      <Button variant="ghost" size="sm" className="ml-auto" disabled={running === list.length} onClick={() => {
        list.filter((task) => task.status !== "running").forEach((task) => dismiss(task.key));
        toggleRef.current?.focus();
      }}>{t("install.clearFinished")}</Button>
    </div>
    <div id={contentId} hidden={!expanded}>
      <div role="separator" className="mx-3 border-t border-dashed border-separator" />
      <p className="px-4 py-2 text-xs leading-relaxed text-muted">{t("install.tasksHint")}</p>
      {!list.length && <p role="status" className="px-4 pb-4 text-xs text-muted">{t("install.noTasks")}</p>}
      <ul aria-label={t("install.tasks")} className="max-h-72 overflow-y-auto overscroll-contain px-3 pb-2">
        {list.map((task) => <InstallTaskRow key={task.key} task={task} onInspect={onInspect}
          onDismiss={() => { dismiss(task.key); toggleRef.current?.focus(); }} />)}
      </ul>
    </div>
  </Card>;
}

function InstallTaskRow({ task, onInspect, onDismiss }: {
  task: InstallTask;
  onInspect: (target: InstallTarget, trigger: HTMLButtonElement | null) => void;
  onDismiss: () => void;
}) {
  const t = useT();
  const progress = useInstallTasks((s) => progressForTask(s.progress, task));
  const cancel = useInstallTasks((s) => s.cancel);
  const busy = task.status === "running";
  const stage = progress ? stageFromState(progress.state) : "download";
  const label = task.status === "error" ? t("install.failed") : task.status === "done" ? t(task.pathSyncError ? "install.pathSyncPending" : "install.stage.done")
    : task.status === "cancelled" ? t("install.cancelled") : task.cancelRequested ? t("install.cancelling")
    : progress ? t(STAGES.find((s) => s.id === stage)?.labelKey ?? "install.stage.download") : t("install.preparing");
  const version = task.resolvedVersion ?? task.version;
  const name = `${task.displayName} ${version ?? t("install.automaticVersion")}`;
  const pct = progress && progress.total > 0 ? Math.min(100, Math.max(0, progress.received / progress.total * 100)) : undefined;
  return <li className="flex flex-wrap items-center gap-3 border-t border-dashed border-separator px-1 py-3 first:border-t-0">
    <div className="min-w-0 flex-1 basis-44 space-y-1">
      <p className="text-xs font-medium [overflow-wrap:anywhere]">{name}</p>
      <p className={cn("flex items-center gap-1.5 text-xs", task.status === "error" ? "text-error" : task.pathSyncError ? "text-warn" : "text-muted")}>
        {busy && <Loader2 className="h-3 w-3 shrink-0 animate-spin" />}{label}
      </p>
      {busy && progress && <div role="progressbar" aria-label={`${name} ${label}`} aria-valuemin={0} aria-valuemax={100}
        aria-valuenow={stage === "download" ? pct : undefined} className="h-1 overflow-hidden rounded-full bg-fill">
        <div className={cn("h-full rounded-full bg-primary", pct === undefined && "w-1/3")} style={pct !== undefined ? { width: `${pct}%` } : undefined} />
      </div>}
      {busy && progress && stage === "download" && <p className="text-[11px] tabular text-muted">{fmtBytes(progress.received)}{progress.total > 0 ? ` / ${fmtBytes(progress.total)}` : ""} · {fmtSpeed(progress.speedBps)}</p>}
      {task.error && <p className="line-clamp-2 text-xs text-error [overflow-wrap:anywhere]">{task.error}</p>}
    </div>
    <div className="flex shrink-0 flex-wrap items-center gap-1">
      <Button size="sm" variant="outline" aria-label={`${t("install.viewTask")} ${name}`} onClick={(event) => onInspect({
        id: task.id, version: task.version, displayName: task.displayName, taskKey: task.key, inspect: true,
      }, event.currentTarget)}>{t("install.viewTask")}</Button>
      {busy ? <Button size="sm" variant="ghost" aria-label={`${t("install.cancel")} ${name}`}
        disabled={task.cancelRequested || stage === "config"} onClick={() => void cancel(task.key)}>{t(task.cancelRequested ? "install.cancelling" : "install.cancel")}</Button>
        : <Button size="icon" variant="ghost" className="h-8 w-8" aria-label={`${t("install.dismissTask")} ${name}`} onClick={onDismiss}><X className="h-3.5 w-3.5" /></Button>}
    </div>
  </li>;
}

/** 供套件页判断：这个包能不能在装完后直接启动 */
export function serviceIdFor(p: Pick<PackageView, "id" | "version" | "run">): string | null {
  if (!p.run) return null;
  return p.run.singleInstance === false ? `${p.id}@${p.version}` : p.id;
}

export type { ServiceStatus };
