"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion } from "motion/react";
import {
  AlertCircle,
  ArrowDownToLine,
  CheckCircle2,
  Download,
  ExternalLink,
  Loader2,
  Package,
  RefreshCw,
  Rocket,
  ShieldCheck,
  Sparkles,
} from "lucide-react";
import type { UpdateCheckResult, UpdateProgress, DownloadUpdateResult } from "@nsb/schema";
import { cn, fmtBytes, fmtSpeed, fmtDuration } from "@/lib/utils";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { isTauri, listen } from "@/lib/backend";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { RingProgress } from "@/components/shared/ring-progress";

type Phase = "checking" | "uptodate" | "available" | "downloading" | "downloaded" | "installing" | "unknown" | "error";

/**
 * 检查更新弹窗：检查 → 有新版本 → 在线下载（带进度）→ 立刻安装。
 *
 * 之前的实现只有一个 toast，用户根本看不出「有没有新版、新版有什么」。
 * 这里把整条链路放进一个弹窗，状态机驱动 UI：
 *   checking → (uptodate | available | unknown | error)
 *   available → downloading → downloaded → installing
 */
export function UpdateDialog({
  open,
  onOpenChange,
  autoCheck = false,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 打开时立刻检查（启动自动检查用） */
  autoCheck?: boolean;
}) {
  const t = useT();
  const [phase, setPhase] = React.useState<Phase>("checking");
  const [result, setResult] = React.useState<UpdateCheckResult | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [dl, setDl] = React.useState<DownloadUpdateResult | null>(null);
  const [progress, setProgress] = React.useState<UpdateProgress | null>(null);

  const check = React.useCallback(
    async (silent = false) => {
      setPhase("checking");
      setError(null);
      setProgress(null);
      setDl(null);
      try {
        const r = await api.checkUpdates();
        setResult(r);
        if (r.appUpdate === null && r.manifestUpdate === null) setPhase("unknown");
        else if (r.appUpdate || r.manifestUpdate) setPhase("available");
        else setPhase("uptodate");
      } catch (e) {
        setError(String((e as { message?: string })?.message ?? e));
        setPhase("error");
        if (!silent) toastError(e);
      }
    },
    []
  );

  // 打开即检查（每次打开都重新拉一次，避免看到上次的陈旧结论）
  React.useEffect(() => {
    if (open && (autoCheck || true)) void check();
  }, [open, autoCheck, check]);

  // 下载进度事件
  React.useEffect(() => {
    if (!open) return;
    let un: (() => void) | undefined;
    listen<UpdateProgress>("update://progress", (p) => setProgress(p)).then((u) => (un = u));
    return () => un?.();
  }, [open]);

  const startDownload = async () => {
    const rel = result?.release;
    if (!rel?.assetUrl) {
      // 没有可自动下载的资产（仓库还没发带安装包的 Release）→ 退到浏览器
      api.openInBrowser(result?.releaseUrl ?? "").catch(toastError);
      return;
    }
    setPhase("downloading");
    setProgress({ received: 0, total: rel.assetSize ?? 0, speedBps: 0, etaSec: 0, state: "downloading" });
    try {
      const r = await api.downloadUpdate(rel.assetUrl, rel.tag, rel.assetName);
      setDl(r);
      setPhase("downloaded");
      toast.success(t("update.downloadDone"));
    } catch (e) {
      setPhase("available");
      setProgress(null);
      toastError(e, t("update.downloadFailed"));
    }
  };

  const install = async () => {
    if (!dl) return;
    setPhase("installing");
    try {
      await api.installUpdate(dl.path);
      // 安装器已拉起、应用即将退出：这里不必再改状态
    } catch (e) {
      setPhase("downloaded");
      toastError(e, t("update.installFailed"));
    }
  };

  const rel = result?.release;
  const pct = progress && progress.total > 0 ? (progress.received / progress.total) * 100 : 0;

  return (
    <Dialog open={open} onOpenChange={(o) => (phase === "downloading" ? undefined : onOpenChange(o))}>
      <DialogContent className="max-w-xl overflow-hidden p-0" hideClose={phase === "downloading"}>
        {/* 头部：图标 + 标题 + 版本 */}
        <div className="relative border-b border-border bg-card-2/30 px-6 py-5">
          <div className="absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-primary/40 to-transparent" />
          <div className="flex items-start gap-4">
            <div
              className={cn(
                "flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl border",
                phase === "available" || phase === "downloaded"
                  ? "border-primary/30 bg-primary-soft"
                  : "border-border bg-card"
              )}
            >
              {phase === "checking" ? (
                <Loader2 className="h-5 w-5 animate-spin text-faint" />
              ) : phase === "uptodate" ? (
                <CheckCircle2 className="h-5 w-5 text-running" />
              ) : phase === "error" || phase === "unknown" ? (
                <AlertCircle className="h-5 w-5 text-warn" />
              ) : phase === "installing" ? (
                <Rocket className="h-5 w-5 text-primary" />
              ) : (
                <Sparkles className="h-5 w-5 text-primary" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <DialogTitle className="text-[15px]">
                {phase === "checking"
                  ? t("update.checking")
                  : phase === "uptodate"
                    ? t("update.upToDate")
                    : phase === "unknown"
                      ? t("settings.updateUnknown")
                      : phase === "error"
                        ? t("update.checkFailed")
                        : t("update.newVersion")}
              </DialogTitle>
              <DialogDescription className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[12px]">
                {result && (
                  <>
                    <span className="font-mono">v{result.appVersion}</span>
                    {phase !== "checking" && phase !== "error" && (
                      <>
                        <span className="text-faint">→</span>
                        <span
                          className={cn(
                            "font-mono font-medium",
                            phase === "available" || phase === "downloading" || phase === "downloaded"
                              ? "text-primary"
                              : "text-faint"
                          )}
                        >
                          v{result.latestVersion ?? "?"}
                        </span>
                      </>
                    )}
                  </>
                )}
              </DialogDescription>
            </div>
          </div>
        </div>

        {/* 主体 */}
        <div className="max-h-[52vh] overflow-y-auto px-6 py-5">
          {phase === "checking" && (
            <div className="flex flex-col gap-2">
              {[0, 1, 2].map((i) => (
                <div key={i} className="h-3 animate-pulse rounded bg-card-2" style={{ width: `${90 - i * 18}%` }} />
              ))}
            </div>
          )}

          {phase === "error" && (
            <div className="flex flex-col gap-2">
              <p className="text-[13px] text-error">{error}</p>
              <p className="text-[11.5px] text-faint">{t("update.checkFailedHint")}</p>
            </div>
          )}

          {phase === "unknown" && (
            <div className="flex flex-col gap-2">
              <p className="text-[13px] text-secondary">{t("settings.updateUnknown")}</p>
              <p className="text-[11.5px] text-faint">{t("update.unknownHint")}</p>
            </div>
          )}

          {phase === "uptodate" && (
            <div className="flex flex-col gap-3">
              <p className="text-[13px] text-secondary">{t("update.upToDateHint")}</p>
              {result && (
                <div className="grid grid-cols-2 gap-2">
                  <InfoTile icon={Package} label={t("update.appVersion")} value={`v${result.appVersion}`} />
                  <InfoTile
                    icon={ShieldCheck}
                    label={t("settings.manifestRev")}
                    value={String(result.manifestRevision)}
                  />
                </div>
              )}
            </div>
          )}

          {(phase === "available" || phase === "downloading" || phase === "downloaded" || phase === "installing") && (
            <div className="flex flex-col gap-4">
              {/* 版本信息 */}
              <div className="grid grid-cols-2 gap-2">
                <InfoTile icon={Package} label={t("update.currentVersion")} value={`v${result?.appVersion ?? "?"}`} />
                <InfoTile
                  icon={ArrowDownToLine}
                  label={t("update.latestVersion")}
                  value={`v${result?.latestVersion ?? "?"}`}
                  highlight
                />
                {rel?.assetSize ? (
                  <InfoTile icon={Download} label={t("update.packageSize")} value={fmtBytes(rel.assetSize)} />
                ) : null}
                {rel?.publishedAt ? (
                  <InfoTile
                    icon={RefreshCw}
                    label={t("update.publishedAt")}
                    value={new Date(rel.publishedAt).toLocaleDateString()}
                  />
                ) : null}
              </div>

              {/* 更新说明 */}
              {rel?.body ? (
                <div className="overflow-hidden rounded-xl bg-fill">
                  <div className="border-b border-border px-3 py-1.5 text-[11px] font-medium text-secondary">
                    {t("update.changelog")}
                  </div>
                  <div className="max-h-44 overflow-y-auto px-3 py-2">
                    <MarkdownLite text={rel.body} />
                  </div>
                </div>
              ) : null}

              {/* 下载进度 */}
              {(phase === "downloading" || phase === "downloaded") && progress && (
                <div className="flex items-center gap-4 rounded-xl border border-primary/25 bg-primary-soft/50 p-3">
                  <RingProgress value={phase === "downloaded" ? 100 : pct} size={54} strokeWidth={5}>
                    <span className="text-[11px] font-semibold tabular text-primary">
                      {phase === "downloaded" ? "100" : pct.toFixed(0)}%
                    </span>
                  </RingProgress>
                  <div className="flex min-w-0 flex-1 flex-col gap-1">
                    <div className="flex items-center justify-between gap-2 text-[11.5px]">
                      <span className="truncate font-mono text-secondary">
                        {rel?.assetName ?? "NiceEnv setup"}
                      </span>
                      <span className="shrink-0 tabular text-faint">
                        {fmtBytes(progress.received)}
                        {progress.total > 0 ? ` / ${fmtBytes(progress.total)}` : ""}
                      </span>
                    </div>
                    <div className="h-1.5 overflow-hidden rounded-full bg-card-2">
                      <motion.div
                        className="h-full rounded-full bg-primary"
                        animate={{ width: `${phase === "downloaded" ? 100 : pct}%` }}
                        transition={{ duration: 0.25 }}
                      />
                    </div>
                    <span className="tabular text-[10.5px] text-faint">
                      {phase === "downloaded"
                        ? t("update.readyToInstall")
                        : `${fmtSpeed(progress.speedBps)} · ${t("packages.eta")} ${fmtDuration(progress.etaSec)}`}
                    </span>
                  </div>
                </div>
              )}

              {phase === "installing" && (
                <div className="flex items-center gap-3 rounded-xl border border-primary/25 bg-primary-soft/50 p-3">
                  <Loader2 className="h-5 w-5 animate-spin text-primary" />
                  <div className="flex flex-col">
                    <span className="text-[12.5px] font-medium text-secondary">{t("update.installing")}</span>
                    <span className="text-[11px] text-faint">{t("update.installingHint")}</span>
                  </div>
                </div>
              )}

              {!isTauri && (
                <p className="rounded-md bg-fill px-3 py-2 text-[11px] text-faint">
                  {t("update.browserMode")}
                </p>
              )}
            </div>
          )}
        </div>

        {/* 底部动作 */}
        <DialogFooter className="border-t border-border bg-card-2/20 px-6 py-3.5">
          {phase === "available" && (
            <>
              <Button variant="ghost" onClick={() => api.openInBrowser(result?.releaseUrl ?? "").catch(toastError)}>
                <ExternalLink className="h-3.5 w-3.5" /> {t("appmenu.releases")}
              </Button>
              <Button variant="ghost" onClick={() => onOpenChange(false)}>
                {t("update.later")}
              </Button>
              <Button onClick={startDownload} disabled={!rel?.assetUrl}>
                <Download className="h-3.5 w-3.5" />
                {rel?.assetUrl ? t("update.downloadNow") : t("update.openRelease")}
              </Button>
            </>
          )}
          {phase === "downloading" && (
            <span className="text-[11.5px] text-faint">{t("update.downloadingHint")}</span>
          )}
          {phase === "downloaded" && (
            <>
              <Button variant="ghost" onClick={() => onOpenChange(false)}>
                {t("update.later")}
              </Button>
              <Button variant="secondary" onClick={() => api.openUpdateDir().catch(toastError)}>
                <ExternalLink className="h-3.5 w-3.5" /> {t("common.open")}
              </Button>
              <Button onClick={install}>
                <Rocket className="h-3.5 w-3.5" /> {t("update.installNow")}
              </Button>
            </>
          )}
          {phase === "installing" && <span className="text-[11.5px] text-faint">{t("update.installingHint")}</span>}
          {(phase === "uptodate" || phase === "unknown" || phase === "error") && (
            <>
              <Button variant="ghost" onClick={() => onOpenChange(false)}>
                {t("common.close")}
              </Button>
              <Button variant="secondary" onClick={() => check()}>
                <RefreshCw className="h-3.5 w-3.5" /> {t("settings.checkUpdate")}
              </Button>
            </>
          )}
          {phase === "checking" && <span className="text-[11.5px] text-faint">{t("settings.checking")}</span>}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function InfoTile({
  icon: Icon,
  label,
  value,
  highlight,
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: string;
  highlight?: boolean;
}) {
  return (
    <div
      className={cn(
        "flex items-center gap-2.5 rounded-xl border px-3 py-2",
        highlight ? "border-primary/25 bg-primary-soft/40" : "border-border bg-card-2/25"
      )}
    >
      <Icon className={cn("h-3.5 w-3.5 shrink-0", highlight ? "text-primary" : "text-faint")} />
      <div className="flex min-w-0 flex-col">
        <span className="text-[10px] text-faint">{label}</span>
        <span className="truncate font-mono text-[12px] text-secondary">{value}</span>
      </div>
    </div>
  );
}

/**
 * 极简 Markdown 渲染：只处理 Release 说明里真会出现的语法
 * （标题 / 列表 / 粗体 / 行内代码 / 链接 / 引用），避免为一段更新日志引入完整 md 解析器。
 */
export function MarkdownLite({ text, className }: { text: string; className?: string }) {
  const blocks = React.useMemo(() => text.replace(/\r\n?/g, "\n").split("\n"), [text]);
  return (
    <div className={cn("flex flex-col gap-1 text-[11.5px] leading-relaxed text-secondary", className)}>
      {blocks.map((raw, i) => {
        const line = raw.trimEnd();
        if (!line.trim()) return <div key={i} className="h-1" />;
        if (/^#{1,6}\s/.test(line)) {
          const depth = line.match(/^#+/)![0].length;
          return (
            <p key={i} className={cn("font-semibold text-foreground", depth <= 2 ? "text-[13px]" : "text-[12px]")}>
              {inline(line.replace(/^#{1,6}\s*/, ""))}
            </p>
          );
        }
        if (/^[-*+]\s/.test(line)) {
          return (
            <div key={i} className="flex gap-2 pl-1">
              <span className="mt-[6px] h-1 w-1 shrink-0 rounded-full bg-faint" />
              <span className="min-w-0 flex-1">{inline(line.replace(/^[-*+]\s*/, ""))}</span>
            </div>
          );
        }
        if (/^>\s?/.test(line)) {
          return (
            <p key={i} className="border-l-2 border-border pl-2 text-faint">
              {inline(line.replace(/^>\s?/, ""))}
            </p>
          );
        }
        return <p key={i}>{inline(line)}</p>;
      })}
    </div>
  );
}

/** 行内语法：`code`、**bold**、[text](url) */
function inline(s: string): React.ReactNode {
  const parts: React.ReactNode[] = [];
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\[[^\]]+\]\([^)]+\))/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let k = 0;
  while ((m = re.exec(s)) !== null) {
    if (m.index > last) parts.push(s.slice(last, m.index));
    const raw = m[0];
    if (raw.startsWith("`")) {
      parts.push(
        <code key={k++} className="rounded bg-card-2 px-1 py-px font-mono text-[11px] text-info">
          {raw.slice(1, -1)}
        </code>
      );
    } else if (raw.startsWith("**")) {
      parts.push(
        <b key={k++} className="font-semibold text-foreground">
          {raw.slice(2, -2)}
        </b>
      );
    } else {
      const mm = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(raw)!;
      parts.push(
        <span key={k++} className="text-info underline decoration-info/40">
          {mm[1]}
        </span>
      );
    }
    last = m.index + raw.length;
  }
  if (last < s.length) parts.push(s.slice(last));
  return parts.length > 0 ? parts : s;
}
