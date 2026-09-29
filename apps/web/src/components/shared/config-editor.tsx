"use client";

import * as React from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { CodeEditor, type CodeEditorHandle } from "./code-editor";
import { Input } from "@/components/ui/input";
import { toast } from "sonner";
import {
  FileCog,
  Loader2,
  Save,
  RotateCcw,
  RotateCw,
  ShieldCheck,
  Undo2,
  AlertTriangle,
  XCircle,
  ExternalLink,
} from "lucide-react";
import type { ConfigBackup, ConfigFileInfo, ConfigValidation, ServiceStatus, ServiceStopPreview } from "@nsb/schema";
import { useQuery } from "@tanstack/react-query";
import { useT } from "@/lib/store";
import { useInvalidate, toastError, serviceHasProcess } from "@/lib/hooks";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/shared/misc";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";

type ConfigDraft = { content: string; original: string; info: ConfigFileInfo };
// 草稿仅保存在当前窗口内存；路径和版本共同隔离，不把配置内容写进浏览器存储。
const configDrafts = new Map<string, ConfigDraft>();
let draftWarningAttached = false;
const warnBeforeLeaving = (event: BeforeUnloadEvent) => {
  if (configDrafts.size) { event.preventDefault(); event.returnValue = ""; }
};
function syncDraftWarning() {
  if (typeof window === "undefined") return;
  if (configDrafts.size && !draftWarningAttached) {
    window.addEventListener("beforeunload", warnBeforeLeaving); draftWarningAttached = true;
  } else if (!configDrafts.size && draftWarningAttached) {
    window.removeEventListener("beforeunload", warnBeforeLeaving); draftWarningAttached = false;
  }
}
function rememberDraft(key: string, content: string, original: string, info: ConfigFileInfo) {
  if (content === original) configDrafts.delete(key);
  else configDrafts.set(key, { content, original, info });
  syncDraftWarning();
}
function forgetDraft(key: string, expected: ConfigDraft | undefined) {
  if (configDrafts.get(key) === expected) configDrafts.delete(key);
  syncDraftWarning();
}

/** 版本配置必须精确匹配；只有后端定义的共用配置允许忽略服务版本。 */
function configForService(files: ConfigFileInfo[], service: string) {
  const exact = files.find((file) => file.usedByService === service);
  if (exact) return exact;
  const base = service.split("@")[0];
  if (!["nginx", "apache", "mihomo"].includes(base)) return undefined;
  return files.find((file) => file.usedByService === base && !file.kind.includes("@"));
}

/** 单实例服务以基础 ID 注册，也必须核对版本；不能把旧版本配置应用到当前新版。 */
function serviceForConfig(info: ConfigFileInfo, services: ServiceStatus[]) {
  if (!info.usedByService) return undefined;
  const [id, version] = info.usedByService.split("@");
  return services.find((service) => version
    ? (service.id === info.usedByService || service.id === id) && service.version === version
    : service.id === id);
}

/**
 * 配置文件编辑器。
 *
 * 与「打开所在文件夹用记事本改」相比，这里的价值只有一件事：**改坏之前拦住**。
 * - 保存前跑语法校验（nginx 走真的 `nginx -t`），不通过就拒写并把行号指出来；
 * - 允许强制保存，但明确告知在跳过校验（校验器偶有误报，不该把人锁死）；
 * - 每次保存自动备份，可一键回滚。
 */
export function ConfigEditor() {
  const t = useT();
  const [search, setSearch] = React.useState("");
  const [editing, setEditing] = React.useState<ConfigFileInfo | null>(null);
  const searchParams = useSearchParams();
  const requestedService = searchParams.get("service");
  const autoOpened = React.useRef(false);
  const { data: files = [], isPending, isFetching, error, refetch } = useQuery({ queryKey: ["config-files"], queryFn: api.configList });
  const filteredFiles = files.filter((f) => `${f.label} ${f.path} ${f.kind}`.toLowerCase().includes(search.toLowerCase()));
  const requestedFile = requestedService ? configForService(files, requestedService) : undefined;
  const unavailableDrafts = [...configDrafts.values()].filter((draft) => error || !files.some((file) => file.kind === draft.info.kind && file.path === draft.info.path));

  // 配置页在 Suspense 内读取查询参数，同一路由切换实例时也重新定位。
  React.useEffect(() => {
    autoOpened.current = false;
  }, [requestedService]);

  React.useEffect(() => {
    if (!requestedService || autoOpened.current || editing || isPending || error) return;
    if (requestedFile?.exists) {
      autoOpened.current = true;
      setEditing(requestedFile);
    }
  }, [requestedService, requestedFile, editing, isPending, error]);

  return (
    <>
      <Card>
        <CardHeader className="flex-row items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-fill">
            <FileCog className="h-4 w-4 text-primary" strokeWidth={1.8} />
          </div>
          <div>
            <CardTitle className="text-[13px]">{t("cfgeditor.title")}</CardTitle>
            <CardDescription className="text-[11px]">{t("cfgeditor.subtitle")}</CardDescription>
          </div>
        </CardHeader>
        <CardContent>
          {unavailableDrafts.length > 0 && <div className="mb-4 space-y-2 rounded-lg border border-warn/25 bg-warn-soft p-3">
            <p className="text-xs text-muted">{t("cfgeditor.unavailableDrafts")}</p>
            {unavailableDrafts.map((draft) => <Button key={JSON.stringify([draft.info.kind, draft.info.path])} variant="secondary" size="sm" className="h-auto w-full justify-start whitespace-normal py-2 text-left [overflow-wrap:anywhere]" onClick={() => { autoOpened.current = true; setEditing(draft.info); }}>{draft.info.label} · {draft.info.path} · {t("cfgeditor.unsaved")}</Button>)}
          </div>}
          {requestedService && !isPending && !error && !requestedFile?.exists && (
            <div role="status" className="mb-4 space-y-2 rounded-lg border border-warn/25 bg-warn-soft px-3 py-2.5 text-xs [overflow-wrap:anywhere]">
              <p>{t(requestedFile ? "cfgeditor.targetNotGenerated" : "cfgeditor.targetUnavailable").replace("{service}", requestedService)}</p>
              {requestedFile && <p className="font-mono text-[10.5px] text-muted">{requestedFile.path}</p>}
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="secondary" disabled={isFetching} onClick={() => void refetch({ cancelRefetch: false })}>{t("install.retry")}</Button>
                <Button size="sm" variant="ghost" asChild><Link href="/configuration">{t("cfgeditor.showAll")}</Link></Button>
              </div>
            </div>
          )}
          <Input value={search} onChange={(e) => setSearch(e.target.value)} placeholder={t("editor.findFile")} aria-label={t("editor.findFile")} className="mb-4" />
          {isPending ? (
            <p className="flex items-center justify-center gap-2 py-4 text-xs text-muted" role="status"><Loader2 className="h-4 w-4 animate-spin" />{t("common.loading")}</p>
          ) : error ? (
            <div className="space-y-2 py-3 text-xs text-error" role="alert">
              <p className="[overflow-wrap:anywhere]">{normalizeError(error).message}</p>
              <Button variant="secondary" size="sm" onClick={() => void refetch()}>{t("install.retry")}</Button>
            </div>
          ) : files.length === 0 ? (
            <p className="py-4 text-center text-[12px] text-faint">{t("cfgeditor.empty")}</p>
          ) : (
            <div className="space-y-1.5">
              {filteredFiles.length === 0 && <p className="py-6 text-center text-xs text-muted">{t("log.noMatch")}</p>}
              {filteredFiles.map((f) => (
                <button
                  key={f.kind}
                  type="button"
                  onClick={() => { if (f.exists || configDrafts.has(JSON.stringify([f.kind, f.path]))) { autoOpened.current = true; setEditing(f); } }}
                  disabled={!f.exists && !configDrafts.has(JSON.stringify([f.kind, f.path]))}
                  className={cn(
                    "flex w-full items-center gap-3 rounded-lg border px-3 py-2 text-left transition-colors",
                    f.exists || configDrafts.has(JSON.stringify([f.kind, f.path]))
                      ? "border-border/60 hover:border-border-strong hover:bg-card-2/30"
                      : "cursor-not-allowed border-border/40 opacity-55"
                  )}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-[12.5px] font-medium">{f.label}</span>
                      {configDrafts.has(JSON.stringify([f.kind, f.path])) && <Badge variant="outline" className="shrink-0 text-[9.5px] text-warn">{t("cfgeditor.unsaved")}</Badge>}
                      {f.validated && (
                        <Badge variant="outline" className="shrink-0 text-[9.5px] text-running">
                          <ShieldCheck className="mr-0.5 h-3 w-3" />
                          {t("cfgeditor.validated")}
                        </Badge>
                      )}
                      {!f.exists && (
                        <span className="shrink-0 text-[10px] text-faint">
                          {t("cfgeditor.notGenerated")}
                        </span>
                      )}
                    </div>
                    <p className="truncate text-[11px] text-faint">{f.description}</p>
                  </div>
                  {f.sizeBytes > 0 && (
                    <span className="shrink-0 tabular text-[10.5px] text-faint">
                      {(f.sizeBytes / 1024).toFixed(1)} KB
                    </span>
                  )}
                </button>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      {editing && (
        <ConfigEditDialog
          info={editing}
          onClose={() => setEditing(null)}
          // 保存后重新拉一遍元信息（大小会变）
          onSaved={() => void refetch()}
        />
      )}
    </>
  );
}

type ConfigEditDialogProps = {
  info: ConfigFileInfo;
  onClose: () => void;
  onSaved: () => void;
};

export function ConfigEditDialog(props: ConfigEditDialogProps) {
  return <ConfigEditSession key={JSON.stringify([props.info.kind, props.info.path])} {...props} />;
}

function ConfigEditSession({
  info,
  onClose,
  onSaved,
}: ConfigEditDialogProps) {
  const t = useT();
  const translateRef = React.useRef(t);
  translateRef.current = t;
  const invalidate = useInvalidate();
  const draftKey = JSON.stringify([info.kind, info.path]);
  const initialDraft = React.useRef(configDrafts.get(draftKey)).current;
  const [content, setContent] = React.useState(initialDraft?.content ?? "");
  const [original, setOriginal] = React.useState(initialDraft?.original ?? "");
  const [hasLoaded, setHasLoaded] = React.useState(!!initialDraft);
  const [loading, setLoading] = React.useState(true);
  const [saving, setSaving] = React.useState(false);
  const [validating, setValidating] = React.useState(false);
  const [rollingBack, setRollingBack] = React.useState(false);
  const [applying, setApplying] = React.useState(false);
  const [confirmApply, setConfirmApply] = React.useState<(ServiceStopPreview & { restart: boolean }) | null>(null);
  const [loadError, setLoadError] = React.useState<string | null>(null);
  const [actionError, setActionError] = React.useState<string | null>(null);
  const [historyError, setHistoryError] = React.useState(false);
  const [notice, setNotice] = React.useState<string | null>(null);
  const [discard, setDiscard] = React.useState<"close" | "reload" | null>(null);
  const actionRef = React.useRef(false);
  const alive = React.useRef(true);
  const reading = React.useRef(false);
  const readGeneration = React.useRef(0);
  const historyGeneration = React.useRef(0);
  const [validation, setValidation] = React.useState<ConfigValidation | null>(null);
  const [backups, setBackups] = React.useState<ConfigBackup[]>([]);
  const [confirmForce, setConfirmForce] = React.useState(false);
  const [confirmRollback, setConfirmRollback] = React.useState<ConfigBackup | null>(null);
  const editorRef = React.useRef<CodeEditorHandle>(null);

  const busy = saving || validating || rollingBack || applying;

  // 后端按 kind 解析当前目录；恢复旧目录草稿前先核对目标，避免写入同名的新配置。
  const verifyTarget = React.useCallback(async () => {
    const files = await api.configList();
    if (!files.some((file) => file.kind === info.kind && file.path === info.path && file.exists)) {
      throw new Error(translateRef.current("cfgeditor.draftTargetChanged"));
    }
  }, [info.kind, info.path]);

  const refreshHistory = React.useCallback(async () => {
    if (!alive.current) return;
    const request = ++historyGeneration.current;
    try {
      const history = await api.configBackups(info.kind);
      if (!alive.current || request !== historyGeneration.current) return;
      setBackups(history);
      setHistoryError(false);
    } catch {
      if (alive.current && request === historyGeneration.current) setHistoryError(true);
    }
  }, [info.kind]);

  const reload = React.useCallback(async (restoreDraft = false) => {
    if (reading.current) return;
    reading.current = true;
    const request = ++readGeneration.current;
    const previousDraft = configDrafts.get(draftKey);
    setLoading(true);
    setLoadError(null);
    try {
      await verifyTarget();
      if (!alive.current || request !== readGeneration.current) return;
      const value = await api.configRead(info.kind);
      if (!alive.current || request !== readGeneration.current) return;
      const draft = restoreDraft ? configDrafts.get(draftKey) : undefined;
      const restored = draft && draft.content !== value;
      setContent(restored ? draft.content : value);
      setOriginal(restored ? draft.original : value);
      setHasLoaded(true);
      setValidation(null);
      setNotice(restored ? translateRef.current("cfgeditor.draftRestored") : null);
      setActionError(restored && draft.original !== value ? translateRef.current("cfgeditor.draftConflict") : null);
      if (!restored) forgetDraft(draftKey, previousDraft);
      await refreshHistory();
    } catch (e) {
      if (alive.current && request === readGeneration.current) setLoadError(normalizeError(e).message);
    } finally {
      if (alive.current && request === readGeneration.current) { reading.current = false; setLoading(false); }
    }
  }, [draftKey, info.kind, refreshHistory, verifyTarget]);

  React.useEffect(() => {
    alive.current = true;
    void reload(true);
    return () => { alive.current = false; reading.current = false; readGeneration.current++; historyGeneration.current++; };
  }, [reload]);

  const dirty = content !== original;
  const close = () => {
    if (busy || actionRef.current) return;
    if (dirty) setDiscard("close");
    else onClose();
  };
  const requestReload = () => {
    if (actionRef.current || reading.current) return;
    if (dirty) setDiscard("reload");
    else void reload();
  };
  const lineCount = React.useMemo(() => content.split("\n").length, [content]);

  const validate = async () => {
    if (actionRef.current || reading.current || !hasLoaded || loadError) return;
    actionRef.current = true;
    setValidating(true);
    setActionError(null);
    try {
      await verifyTarget();
      if (!alive.current) return;
      const v = await api.configValidate(info.kind, content);
      if (alive.current) setValidation(v);
      if (v.ok) toast.success(t("cfgeditor.checkOk"));
      else toast.error(t("cfgeditor.checkFailed"));
      return v;
    } catch (e) {
      if (alive.current) setActionError(normalizeError(e).message);
      else toastError(e);
      return null;
    } finally {
      actionRef.current = false;
      if (alive.current) setValidating(false);
    }
  };

  const save = async (force = false) => {
    if (actionRef.current || reading.current || !hasLoaded || loadError || !dirty) return;
    actionRef.current = true;
    const submittedDraft = configDrafts.get(draftKey);
    setSaving(true);
    setActionError(null);
    setNotice(null);
    try {
      await verifyTarget();
      if (!alive.current) return;
      if (!force) {
        const checked = await api.configValidate(info.kind, content);
        if (alive.current) setValidation(checked);
        if (!checked.ok) return;
      }
      if (!alive.current) return;
      const v = await api.configSave(info.kind, content, force, original);
      forgetDraft(draftKey, submittedDraft);
      if (alive.current) { setValidation(v); setOriginal(content); }
      toast.success(force ? t("cfgeditor.savedForced") : t("cfgeditor.saved"), {
        description: info.usedByService
          ? t("cfgeditor.restartHint").replace("{s}", info.usedByService)
          : undefined,
      });
      if (alive.current) setNotice(info.usedByService ? t("cfgeditor.restartHint").replace("{s}", info.usedByService) : t("cfgeditor.saved"));
      await refreshHistory();
      invalidate("services", "backups", "config-files");
      if (alive.current) onSaved();
    } catch (e) {
      const err = normalizeError(e);
      if (alive.current) setActionError(`${err.message}${err.hint ? ` — ${err.hint}` : ""}`);
      else toastError(e);
    } finally {
      actionRef.current = false;
      if (alive.current) setSaving(false);
    }
  };

  const rollback = async (b: ConfigBackup) => {
    if (actionRef.current || reading.current || !hasLoaded || loadError) return;
    actionRef.current = true;
    const discardedDraft = configDrafts.get(draftKey);
    setRollingBack(true);
    setActionError(null);
    try {
      await verifyTarget();
      if (!alive.current) return;
      await api.configRollback(b.name, info.kind, original);
      forgetDraft(draftKey, discardedDraft);
      toast.success(t("cfgeditor.rolledBack"));
      if (alive.current) {
        await reload();
        if (alive.current) setNotice(info.usedByService ? t("cfgeditor.restartHint").replace("{s}", info.usedByService) : t("cfgeditor.rolledBack"));
      }
      invalidate("services", "backups", "config-files");
      if (alive.current) onSaved();
    } catch (e) {
      const err = normalizeError(e);
      if (alive.current) setActionError(`${err.message}${err.hint ? ` — ${err.hint}` : ""}`);
      else toastError(e);
    } finally {
      actionRef.current = false;
      if (alive.current) setRollingBack(false);
    }
  };

  const prepareApply = async () => {
    if (actionRef.current || reading.current || !hasLoaded || loadError || dirty || !info.usedByService) return;
    actionRef.current = true;
    setApplying(true);
    setActionError(null);
    try {
      await verifyTarget();
      const service = serviceForConfig(info, await api.listServiceStatus());
      if (!service) throw new Error(t("cfgeditor.applyServiceMissing"));
      const preview = await api.serviceStopPreview(service.id);
      if (!serviceForConfig(info, [preview.service])) throw new Error(t("cfgeditor.applyServiceMissing"));
      if (["starting", "stopping", "unknown"].includes(preview.service.state)) throw new Error(t("cfgeditor.applyServiceBusy"));
      if (preview.service.missingRequires.length) throw new Error(t("svc.needDepsHint"));
      if (await api.configRead(info.kind) !== original) throw new Error(t("cfgeditor.applyConflict"));
      if (alive.current) setConfirmApply({ ...preview, restart: serviceHasProcess(preview.service) });
    } catch (e) {
      if (alive.current) setActionError(normalizeError(e).message);
      else toastError(e);
    } finally {
      actionRef.current = false;
      if (alive.current) setApplying(false);
    }
  };

  const apply = async () => {
    if (!confirmApply || actionRef.current || reading.current || !hasLoaded || loadError || dirty) return;
    actionRef.current = true;
    setApplying(true);
    setActionError(null);
    try {
      await api.configApply(info.kind, info.path, original, confirmApply.service.id, confirmApply.revision, confirmApply.restart);
      const message = t(confirmApply.restart ? "cfgeditor.appliedRestart" : "cfgeditor.appliedStart").replace("{s}", confirmApply.service.label);
      toast.success(message);
      if (alive.current) {
        await reload();
        if (alive.current) setNotice(message);
      }
    } catch (e) {
      const err = normalizeError(e);
      if (alive.current) { setNotice(null); setActionError(`${err.message}${err.hint ? ` — ${err.hint}` : ""}`); }
      else toastError(e);
    } finally {
      invalidate("services", "sites", "stacks", "config-files");
      actionRef.current = false;
      if (alive.current) { setApplying(false); setConfirmApply(null); }
    }
  };

  const errors = validation?.issues.filter((i) => i.severity === "error") ?? [];
  const warnings = validation?.issues.filter((i) => i.severity === "warning") ?? [];

  const jumpTo = (line: number) => editorRef.current?.jumpToLine(line);

  return (
    <>
      <Dialog open onOpenChange={(v) => !v && close()}>
        <DialogContent hideClose={busy} className="flex h-[88dvh] max-h-[calc(100dvh-24px)] max-w-5xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 border-b border-border py-3.5 pl-4 pr-12 sm:pl-5">
            <div className="flex flex-wrap items-center gap-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border border-primary/30 bg-primary-soft">
                <FileCog className="h-[17px] w-[17px] text-primary" strokeWidth={1.8} />
              </div>
              <div className="min-w-0 flex-1">
                <DialogTitle className="text-[14.5px] leading-snug [overflow-wrap:anywhere]">
                  {info.label}
                  {dirty && (
                    <span className="ml-2 align-middle text-[11px] font-normal text-warn">
                      ● {t("cfgeditor.unsaved")}
                    </span>
                  )}
                </DialogTitle>
                <DialogDescription className="max-h-16 overflow-y-auto font-mono text-[10.5px] [overflow-wrap:anywhere]">
                  {info.path}
                </DialogDescription>
              </div>
              <div className="flex w-full flex-wrap items-center gap-1.5 sm:w-auto">
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-8"
                  onClick={() => void api.openInFolder(info.path).catch(toastError)}
                  title={t("cfgeditor.reveal")}
                  aria-label={t("cfgeditor.reveal")}
                >
                  <ExternalLink className="h-3.5 w-3.5" />
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  className="h-8"
                  onClick={() => void validate()}
                  disabled={loading || busy || !!loadError}
                >
                  {validating ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <ShieldCheck className="h-3.5 w-3.5" />}
                  <span className="ml-1.5">{t("cfgeditor.check")}</span>
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-8"
                  onClick={requestReload}
                  disabled={loading || busy}
                  title={t("cfgeditor.reload")}
                  aria-label={t("cfgeditor.reload")}
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                </Button>
              </div>
            </div>
          </DialogHeader>

          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
            <p className="shrink-0 border-b border-border px-4 py-2 text-[11px] text-muted [overflow-wrap:anywhere] sm:px-5">{info.description}</p>
            {loadError && hasLoaded && <div role="alert" className="shrink-0 space-y-2 bg-error-soft px-4 py-2.5 text-xs text-error [overflow-wrap:anywhere] sm:px-5">
              <p>{t("cfgeditor.reloadKeptDraft")}</p><p>{loadError}</p>
              <Button variant="secondary" size="sm" disabled={loading || busy} onClick={() => void reload(true)}>{t("install.retry")}</Button>
            </div>}
            {loading && hasLoaded && <p role="status" className="shrink-0 px-4 py-2 text-xs text-muted sm:px-5">{t("common.loading")}</p>}
            {(actionError || notice) && (
              <p role={actionError ? "alert" : "status"} className={cn("max-h-24 shrink-0 overflow-y-auto px-4 py-2 text-xs [overflow-wrap:anywhere] sm:px-5", actionError ? "bg-error-soft text-error" : "bg-running-soft text-secondary")}>
                {actionError || notice}
              </p>
            )}

            {/* 校验结果条：错误可点击跳到对应行 */}
            {validation && (errors.length > 0 || warnings.length > 0) && (
              <div
                className={cn(
                  "max-h-[22dvh] shrink-0 overflow-y-auto border-b px-4 py-2.5 sm:px-5",
                  errors.length > 0
                    ? "border-error/25 bg-error-soft"
                    : "border-warn/25 bg-warn-soft"
                )}
              >
                <div className="flex flex-wrap items-start gap-2">
                  {errors.length > 0 ? (
                    <XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-error" strokeWidth={2} />
                  ) : (
                    <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
                  )}
                  <div className="min-w-0 flex-1 space-y-0.5">
                    {[...errors, ...warnings].map((iss, i) => (
                      <button
                        key={i}
                        type="button"
                        onClick={() => jumpTo(iss.line)}
                        className="block w-full text-left text-[11.5px] leading-relaxed [overflow-wrap:anywhere] hover:underline"
                      >
                        {iss.line > 0 && (
                          <span className="mr-1.5 font-mono text-faint">
                            {t("cfgeditor.line")} {iss.line}
                          </span>
                        )}
                        <span className={iss.severity === "error" ? "text-error" : "text-warn"}>
                          {iss.message}
                        </span>
                      </button>
                    ))}
                  </div>
                  {errors.length > 0 && (
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-6 shrink-0 text-[11px] text-error"
                      onClick={() => setConfirmForce(true)}
                      disabled={busy || loading || !!loadError || !dirty}
                    >
                      {t("cfgeditor.forceSave")}
                    </Button>
                  )}
                </div>
              </div>
            )}

            {validation && validation.messages.length > 0 && (
              <details className="max-h-28 shrink-0 overflow-y-auto border-b border-border px-4 py-2 text-[11px] sm:px-5">
                <summary className="cursor-pointer text-muted">{t("cfgeditor.validationDetails")}</summary>
                <pre className="mt-2 whitespace-pre-wrap font-mono [overflow-wrap:anywhere]">{validation.messages.join("\n")}</pre>
              </details>
            )}

            {/* 编辑器：搜索、语法高亮与可跳转行号 */}
            <div className="flex min-h-44 flex-1 overflow-hidden bg-card-2/20">
              {loading && !hasLoaded ? (
                <div className="flex flex-1 items-center justify-center">
                  <Loader2 className="h-5 w-5 animate-spin text-primary" />
                </div>
              ) : loadError && !hasLoaded ? (
                <div className="flex min-w-0 flex-1 flex-col items-center justify-center gap-3 overflow-y-auto p-4 text-xs" role="alert">
                  <p className="text-error [overflow-wrap:anywhere]">{loadError}</p>
                  <Button variant="secondary" onClick={() => void reload()}>{t("install.retry")}</Button>
                </div>
              ) : (
                <div className="min-w-0 flex-1 overflow-auto p-2"><CodeEditor ref={editorRef} value={content} label={info.label} language={info.kind.startsWith("apache") ? "apache" : info.kind.startsWith("php") || info.kind.startsWith("mysql") || info.kind.startsWith("mariadb") || info.kind.startsWith("postgres") ? "ini" : info.kind.startsWith("mongo") ? "yaml" : info.path} readOnly={busy || loading} height="min(52dvh, 520px)" onChange={(next) => {
                  if (actionRef.current || reading.current) return;
                  rememberDraft(draftKey, next, original, info);
                  setContent(next); setValidation(null); setNotice(null);
                }} /></div>
              )}
            </div>

            {/* 备份历史 */}
            {historyError && <p role="status" className="shrink-0 px-4 py-2 text-[11px] text-warn">{t("cfgeditor.historyError")}</p>}
            {backups.length > 0 && (
              <details className="max-h-28 shrink-0 overflow-y-auto border-t border-border px-4 py-2.5 sm:px-5">
                <summary className="cursor-pointer text-[11px] text-muted">
                  <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                    {t("cfgeditor.backups")}
                  </span>
                  <span className="ml-2">{backups.length}</span>
                </summary>
                <div className="mt-1.5 flex flex-wrap gap-1.5">
                  {backups.map((b) => (
                    <button
                      key={b.name}
                      type="button"
                      onClick={() => setConfirmRollback(b)}
                      disabled={busy || loading || !!loadError}
                      className="inline-flex min-h-8 items-center gap-1 rounded-md border border-border/60 bg-card-2/40 px-2 py-1 font-mono text-[10.5px] text-muted transition-colors hover:border-border-strong hover:text-foreground disabled:opacity-50"
                      title={`${info.label} · ${new Date(b.createdAt * 1000).toLocaleString()}`}
                    >
                      <Undo2 className="h-3 w-3" />
                      {new Date(b.createdAt * 1000).toLocaleString()}
                    </button>
                  ))}
                </div>
              </details>
            )}
          </div>
          <DialogFooter className="shrink-0 flex-wrap border-t border-border px-4 py-3 sm:px-5">
            <Button variant="ghost" onClick={close} disabled={busy}>{t("common.close")}</Button>
            {info.usedByService && <Button variant="secondary" onClick={() => void prepareApply()} disabled={dirty || busy || loading || !!loadError || !hasLoaded} title={dirty ? t("cfgeditor.applySaveFirst") : t("cfgeditor.apply")}>
              {applying ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <RotateCw className="h-3.5 w-3.5" />}
              {t("cfgeditor.apply")}
            </Button>}
            <Button onClick={() => void save(false)} disabled={!dirty || busy || loading || !!loadError}>
              {saving || rollingBack ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
              {t("common.save")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog open={confirmApply !== null} onOpenChange={(open) => { if (!open && !actionRef.current) setConfirmApply(null); }}
        title={t(confirmApply?.restart ? "cfgeditor.applyRestartTitle" : "cfgeditor.applyStartTitle")}
        description={t(confirmApply?.restart ? "cfgeditor.applyRestartDesc" : "cfgeditor.applyStartDesc").replace("{s}", confirmApply ? `${confirmApply.service.label}${confirmApply.service.version ? ` · ${confirmApply.service.version}` : ""}` : "")}
        confirmText={t(confirmApply?.restart ? "cfgeditor.applyRestart" : "cfgeditor.applyStart")}
        loading={applying} confirmDisabled={dirty || loading || !!loadError} onConfirm={() => void apply()}>
        <p className="text-xs text-muted [overflow-wrap:anywhere]">{info.path}</p>
      </ConfirmDialog>

      <ConfirmDialog open={discard !== null} onOpenChange={(open) => !open && setDiscard(null)}
        title={t("cfgeditor.discardTitle")} description={t("cfgeditor.discardDesc")}
        confirmText={t("cfgeditor.discard")} danger onConfirm={() => {
          const action = discard;
          setDiscard(null);
          if (action === "close") { forgetDraft(draftKey, configDrafts.get(draftKey)); onClose(); }
          else void reload();
        }} />

      <ConfirmDialog
        open={confirmForce}
        onOpenChange={(v) => !v && setConfirmForce(false)}
        title={t("cfgeditor.forceTitle")}
        description={t("cfgeditor.forceDesc")}
        confirmText={t("cfgeditor.forceSave")}
        danger
        onConfirm={() => {
          setConfirmForce(false);
          void save(true);
        }}
      />

      <ConfirmDialog
        open={confirmRollback != null}
        onOpenChange={(v) => !v && setConfirmRollback(null)}
        title={t("cfgeditor.rollbackTitle")}
        description={`${t("cfgeditor.rollbackDesc").replace("{n}", confirmRollback ? `${info.label} · ${new Date(confirmRollback.createdAt * 1000).toLocaleString()}` : "")}${dirty ? ` ${t("cfgeditor.discardDesc")}` : ""}`}
        confirmText={t("cfgeditor.rollback")}
        danger
        onConfirm={() => {
          const b = confirmRollback;
          setConfirmRollback(null);
          if (b) void rollback(b);
        }}
      />
    </>
  );
}
