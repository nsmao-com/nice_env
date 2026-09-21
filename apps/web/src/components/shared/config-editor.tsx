"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  FileCog,
  Loader2,
  Save,
  RotateCcw,
  ShieldCheck,
  ShieldAlert,
  Undo2,
  AlertTriangle,
  XCircle,
  ExternalLink,
} from "lucide-react";
import type { ConfigBackup, ConfigFileInfo, ConfigValidation } from "@nsb/schema";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
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
  const [files, setFiles] = React.useState<ConfigFileInfo[]>([]);
  const [editing, setEditing] = React.useState<ConfigFileInfo | null>(null);

  const load = React.useCallback(async () => {
    try {
      setFiles(await api.configList());
    } catch {
      /* 服务未就绪时不报错，列表留空 */
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

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
          {files.length === 0 ? (
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
                    <div className="flex items-center gap-2">
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
          onSaved={() => void load()}
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
  const [validation, setValidation] = React.useState<ConfigValidation | null>(null);
  const [backups, setBackups] = React.useState<ConfigBackup[]>([]);
  const [confirmForce, setConfirmForce] = React.useState(false);
  const [confirmRollback, setConfirmRollback] = React.useState<ConfigBackup | null>(null);
  const taRef = React.useRef<HTMLTextAreaElement>(null);
  const gutterRef = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    void (async () => {
      try {
        const [c, b] = await Promise.all([
          api.configRead(info.kind),
          api.configBackups().catch(() => []),
        ]);
        setContent(c);
        setOriginal(c);
        setBackups(b);
      } catch (e) {
        toastError(e);
        onClose();
      } finally {
        setLoading(false);
      }
    })();
  }, [info.kind, onClose]);

  const dirty = content !== original;
  const lineCount = React.useMemo(() => content.split("\n").length, [content]);

  // 同步行号槽的滚动位置（textarea 自己滚，行号得跟着）
  const onScroll = () => {
    if (gutterRef.current && taRef.current) {
      gutterRef.current.scrollTop = taRef.current.scrollTop;
    }
  };

  const validate = async () => {
    try {
      const v = await api.configValidate(info.kind, content);
      setValidation(v);
      if (v.ok) toast.success(t("cfgeditor.checkOk"));
      else toast.error(t("cfgeditor.checkFailed"));
      return v;
    } catch (e) {
      toastError(e);
      return null;
    }
  };

  const save = async (force = false) => {
    setSaving(true);
    try {
      const v = await api.configSave(info.kind, content, force);
      setValidation(v);
      setOriginal(content);
      toast.success(force ? t("cfgeditor.savedForced") : t("cfgeditor.saved"), {
        description: info.usedByService
          ? t("cfgeditor.restartHint").replace("{s}", info.usedByService)
          : undefined,
      });
      const b = await api.configBackups().catch(() => []);
      setBackups(b);
      invalidate("services");
      onSaved();
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  const rollback = async (b: ConfigBackup) => {
    try {
      await api.configRollback(b.name);
      toast.success(t("cfgeditor.rolledBack"));
      const c = await api.configRead(info.kind);
      setContent(c);
      setOriginal(c);
      const list = await api.configBackups().catch(() => []);
      setBackups(list);
    } catch (e) {
      toastError(e);
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
      <Dialog open onOpenChange={(v) => !v && onClose()}>
        <DialogContent className="flex h-[88vh] max-w-5xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 border-b border-border px-5 py-3.5">
            <div className="flex items-center gap-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border border-primary/30 bg-primary-soft">
                <FileCog className="h-[17px] w-[17px] text-primary" strokeWidth={1.8} />
              </div>
              <div className="min-w-0 flex-1">
                <DialogTitle className="text-[14.5px]">
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
              <div className="flex shrink-0 items-center gap-1.5">
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-8"
                  onClick={() => void api.openInFolder(info.path).catch(toastError)}
                  title={t("cfgeditor.reveal")}
                >
                  <ExternalLink className="h-3.5 w-3.5" />
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  className="h-8"
                  onClick={() => void validate()}
                  disabled={loading || saving}
                >
                  <ShieldCheck className="h-3.5 w-3.5" />
                  <span className="ml-1.5">{t("cfgeditor.check")}</span>
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-8"
                  onClick={() => {
                    setContent(original);
                    setValidation(null);
                  }}
                  disabled={!dirty || saving}
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                </Button>
                <Button
                  size="sm"
                  className="h-8"
                  onClick={() => void save(false)}
                  disabled={!dirty || saving}
                >
                  {saving ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                  <span className="ml-1.5">{t("common.save")}</span>
                </Button>
              </div>
            </div>
          </DialogHeader>

          {/* 校验结果条：错误可点击跳到对应行 */}
          {validation && (errors.length > 0 || warnings.length > 0) && (
            <div
              className={cn(
                "shrink-0 border-b px-5 py-2.5",
                errors.length > 0
                  ? "border-error/25 bg-error-soft"
                  : "border-warn/25 bg-warn-soft"
              )}
            >
              <div className="flex items-start gap-2">
                {errors.length > 0 ? (
                  <XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-error" strokeWidth={2} />
                ) : (
                  <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
                )}
                <div className="min-w-0 flex-1 space-y-0.5">
                  {[...errors, ...warnings].slice(0, 6).map((iss, i) => (
                    <button
                      key={i}
                      type="button"
                      onClick={() => jumpTo(iss.line)}
                      className="block w-full text-left text-[11.5px] leading-relaxed hover:underline"
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
                  >
                    {t("cfgeditor.forceSave")}
                  </Button>
                )}
              </div>
            </div>
          )}

          {/* 编辑器：行号槽 + textarea，共享滚动 */}
          <div className="flex min-h-0 flex-1 overflow-hidden bg-card-2/20">
            {loading ? (
              <div className="flex flex-1 items-center justify-center">
                <Loader2 className="h-5 w-5 animate-spin text-primary" />
              </div>
            ) : (
              <>
                <div
                  ref={gutterRef}
                  aria-hidden
                  className="w-12 shrink-0 select-none overflow-hidden border-r border-border/60 bg-card-2/40 py-2 text-right font-mono text-[11.5px] leading-[20px] text-faint"
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
                  onChange={(e) => {
                    setContent(e.target.value);
                    setValidation(null);
                  }}
                  onScroll={onScroll}
                  spellCheck={false}
                  className="min-h-0 flex-1 resize-none bg-transparent px-3 py-2 font-mono text-[11.5px] leading-[20px] text-foreground outline-none"
                  style={{ tabSize: 2 }}
                />
              </>
            )}
          </div>

          {/* 备份历史 */}
          {backups.length > 0 && (
            <div className="shrink-0 border-t border-border px-5 py-2.5">
              <div className="flex items-center gap-2">
                <span className="text-[10px] font-semibold uppercase tracking-[0.09em] text-faint/70">
                  {t("cfgeditor.backups")}
                </span>
                <span className="h-px flex-1 bg-border/60" />
              </div>
              <div className="mt-1.5 flex flex-wrap gap-1.5">
                {backups.slice(0, 6).map((b) => (
                  <button
                    key={b.name}
                    type="button"
                    onClick={() => setConfirmRollback(b)}
                    className="inline-flex items-center gap-1 rounded-md border border-border/60 bg-card-2/40 px-2 py-0.5 font-mono text-[10.5px] text-muted transition-colors hover:border-border-strong hover:text-foreground"
                    title={b.name}
                  >
                    <Undo2 className="h-3 w-3" />
                    {b.name.slice(0, 28)}
                  </button>
                ))}
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>

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
        description={t("cfgeditor.rollbackDesc").replace("{n}", confirmRollback?.name ?? "")}
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
