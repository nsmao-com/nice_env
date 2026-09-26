"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  FileCog,
  Loader2,
  Save,
  RotateCcw,
  ShieldCheck,
  Undo2,
  AlertTriangle,
  XCircle,
  ExternalLink,
} from "lucide-react";
import type { ConfigBackup, ConfigFileInfo, ConfigValidation } from "@nsb/schema";
import { useQuery } from "@tanstack/react-query";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
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
  const [editing, setEditing] = React.useState<ConfigFileInfo | null>(null);
  const { data: files = [], isPending, error, refetch } = useQuery({ queryKey: ["config-files"], queryFn: api.configList });

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
              {files.map((f) => (
                <button
                  key={f.kind}
                  type="button"
                  onClick={() => f.exists && setEditing(f)}
                  disabled={!f.exists}
                  className={cn(
                    "flex w-full items-center gap-3 rounded-lg border px-3 py-2 text-left transition-colors",
                    f.exists
                      ? "border-border/60 hover:border-border-strong hover:bg-card-2/30"
                      : "cursor-not-allowed border-border/40 opacity-55"
                  )}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-[12.5px] font-medium">{f.label}</span>
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

function ConfigEditDialog({
  info,
  onClose,
  onSaved,
}: {
  info: ConfigFileInfo;
  onClose: () => void;
  onSaved: () => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const [content, setContent] = React.useState("");
  const [original, setOriginal] = React.useState("");
  const [loading, setLoading] = React.useState(true);
  const [saving, setSaving] = React.useState(false);
  const [validating, setValidating] = React.useState(false);
  const [rollingBack, setRollingBack] = React.useState(false);
  const [loadError, setLoadError] = React.useState<string | null>(null);
  const [actionError, setActionError] = React.useState<string | null>(null);
  const [historyError, setHistoryError] = React.useState(false);
  const [notice, setNotice] = React.useState<string | null>(null);
  const [discard, setDiscard] = React.useState<"close" | "reload" | null>(null);
  const actionRef = React.useRef(false);
  const [validation, setValidation] = React.useState<ConfigValidation | null>(null);
  const [backups, setBackups] = React.useState<ConfigBackup[]>([]);
  const [confirmForce, setConfirmForce] = React.useState(false);
  const [confirmRollback, setConfirmRollback] = React.useState<ConfigBackup | null>(null);
  const taRef = React.useRef<HTMLTextAreaElement>(null);
  const gutterRef = React.useRef<HTMLDivElement>(null);
  const busy = saving || validating || rollingBack;

  const refreshHistory = React.useCallback(async () => {
    try {
      setBackups(await api.configBackups(info.kind));
      setHistoryError(false);
    } catch {
      setHistoryError(true);
    }
  }, [info.kind]);

  const reload = React.useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const value = await api.configRead(info.kind);
      setContent(value);
      setOriginal(value);
      setValidation(null);
      setActionError(null);
      await refreshHistory();
    } catch (e) {
      setLoadError(normalizeError(e).message);
    } finally {
      setLoading(false);
    }
  }, [info.kind, refreshHistory]);

  React.useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const c = await api.configRead(info.kind);
        if (!alive) return;
        setContent(c);
        setOriginal(c);
        void refreshHistory();
      } catch (e) {
        if (alive) setLoadError(normalizeError(e).message);
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => { alive = false; };
  }, [info.kind, refreshHistory]);

  const dirty = content !== original;
  const close = () => {
    if (busy || actionRef.current) return;
    if (dirty) setDiscard("close");
    else onClose();
  };
  React.useEffect(() => {
    if (!dirty) return;
    const guard = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", guard);
    return () => window.removeEventListener("beforeunload", guard);
  }, [dirty]);
  const lineCount = React.useMemo(() => content.split("\n").length, [content]);

  // 同步行号槽的滚动位置（textarea 自己滚，行号得跟着）
  const onScroll = () => {
    if (gutterRef.current && taRef.current) {
      gutterRef.current.scrollTop = taRef.current.scrollTop;
    }
  };

  const validate = async () => {
    if (actionRef.current) return;
    actionRef.current = true;
    setValidating(true);
    setActionError(null);
    try {
      const v = await api.configValidate(info.kind, content);
      setValidation(v);
      if (v.ok) toast.success(t("cfgeditor.checkOk"));
      else toast.error(t("cfgeditor.checkFailed"));
      return v;
    } catch (e) {
      setActionError(normalizeError(e).message);
      return null;
    } finally {
      actionRef.current = false;
      setValidating(false);
    }
  };

  const save = async (force = false) => {
    if (actionRef.current) return;
    actionRef.current = true;
    setSaving(true);
    setActionError(null);
    setNotice(null);
    try {
      if (!force) {
        const checked = await api.configValidate(info.kind, content);
        setValidation(checked);
        if (!checked.ok) return;
      }
      const v = await api.configSave(info.kind, content, force, original);
      setValidation(v);
      setOriginal(content);
      toast.success(force ? t("cfgeditor.savedForced") : t("cfgeditor.saved"), {
        description: info.usedByService
          ? t("cfgeditor.restartHint").replace("{s}", info.usedByService)
          : undefined,
      });
      setNotice(info.usedByService ? t("cfgeditor.restartHint").replace("{s}", info.usedByService) : t("cfgeditor.saved"));
      await refreshHistory();
      invalidate("services", "backups");
      onSaved();
    } catch (e) {
      const err = normalizeError(e);
      setActionError(`${err.message}${err.hint ? ` — ${err.hint}` : ""}`);
    } finally {
      actionRef.current = false;
      setSaving(false);
    }
  };

  const rollback = async (b: ConfigBackup) => {
    if (actionRef.current) return;
    actionRef.current = true;
    setRollingBack(true);
    setActionError(null);
    try {
      await api.configRollback(b.name, info.kind, original);
      toast.success(t("cfgeditor.rolledBack"));
      await reload();
      setNotice(info.usedByService ? t("cfgeditor.restartHint").replace("{s}", info.usedByService) : t("cfgeditor.rolledBack"));
      invalidate("services", "backups");
      onSaved();
    } catch (e) {
      const err = normalizeError(e);
      setActionError(`${err.message}${err.hint ? ` — ${err.hint}` : ""}`);
    } finally {
      actionRef.current = false;
      setRollingBack(false);
    }
  };

  const errors = validation?.issues.filter((i) => i.severity === "error") ?? [];
  const warnings = validation?.issues.filter((i) => i.severity === "warning") ?? [];

  const jumpTo = (line: number) => {
    const ta = taRef.current;
    if (!ta || line <= 0) return;
    const lines = content.split("\n");
    let pos = 0;
    for (let i = 0; i < line - 1 && i < lines.length; i++) pos += lines[i].length + 1;
    ta.focus();
    ta.setSelectionRange(pos, pos + (lines[line - 1]?.length ?? 0));
    // 粗算滚动位置，让目标行进入视野
    ta.scrollTop = Math.max(0, (line - 3) * 20);
    onScroll();
  };

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
                <DialogDescription className="truncate font-mono text-[10.5px]">
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
                  onClick={() => {
                    if (dirty) setDiscard("reload");
                    else void reload();
                  }}
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
                      disabled={busy || loading || !dirty}
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

            {/* 编辑器：行号槽 + textarea，共享滚动 */}
            <div className="flex min-h-44 flex-1 overflow-hidden bg-card-2/20">
              {loading ? (
                <div className="flex flex-1 items-center justify-center">
                  <Loader2 className="h-5 w-5 animate-spin text-primary" />
                </div>
              ) : loadError ? (
                <div className="flex min-w-0 flex-1 flex-col items-center justify-center gap-3 overflow-y-auto p-4 text-xs" role="alert">
                  <p className="text-error [overflow-wrap:anywhere]">{loadError}</p>
                  <Button variant="secondary" onClick={() => void reload()}>{t("install.retry")}</Button>
                </div>
              ) : (
                <>
                  <div
                    ref={gutterRef}
                    aria-hidden
                    className="w-9 shrink-0 select-none overflow-hidden border-r border-border/60 bg-card-2/40 py-2 text-right font-mono text-[11.5px] leading-[20px] text-faint sm:w-12"
                  >
                    {Array.from({ length: lineCount }).map((_, i) => {
                      const n = i + 1;
                      const isErr = errors.some((e) => e.line === n);
                      const isWarn = warnings.some((w) => w.line === n);
                      return (
                        <div
                          key={n}
                          className={cn(
                            "pr-2 tabular",
                            isErr && "bg-error/15 font-semibold text-error",
                            !isErr && isWarn && "bg-warn/15 text-warn"
                          )}
                        >
                          {n}
                        </div>
                      );
                    })}
                  </div>
                  <textarea
                    ref={taRef}
                    value={content}
                    aria-label={`${info.label} ${t("cfgeditor.content")}`}
                    readOnly={busy}
                    wrap="off"
                    onChange={(e) => {
                      setContent(e.target.value);
                      setValidation(null);
                      setNotice(null);
                    }}
                    onScroll={onScroll}
                    spellCheck={false}
                    className="min-h-0 min-w-0 flex-1 resize-none bg-transparent px-3 py-2 font-mono text-[11.5px] leading-[20px] text-foreground outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-primary"
                    style={{ tabSize: 2 }}
                  />
                </>
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
          <DialogFooter className="shrink-0 border-t border-border px-4 py-3 sm:px-5">
            <Button variant="ghost" onClick={close} disabled={busy}>{t("common.close")}</Button>
            <Button onClick={() => void save(false)} disabled={!dirty || busy || loading || !!loadError}>
              {saving || rollingBack ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
              {t("common.save")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog open={discard !== null} onOpenChange={(open) => !open && setDiscard(null)}
        title={t("cfgeditor.discardTitle")} description={t("cfgeditor.discardDesc")}
        confirmText={t("cfgeditor.discard")} danger onConfirm={() => {
          const action = discard;
          setDiscard(null);
          if (action === "close") onClose();
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
