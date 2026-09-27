"use client";

import * as React from "react";
import { Eye, EyeOff, Plus, RefreshCw, Undo2, Wand2 } from "lucide-react";
import type { EnvFileView } from "@nsb/schema";
import { useT } from "@/lib/store";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn, isEnvSecretKey } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/misc";
import { ConfirmDialog } from "@/components/shared/misc";

export type EnvEditorState = { dirty: boolean; busy: boolean; canSave: boolean };
export type EnvEditorHandle = { save: () => Promise<void>; isBusy: () => boolean };

/** 文件与站点设置分开保存，父面板的固定底栏负责当前页签的保存动作。 */
export const EnvEditor = React.forwardRef<EnvEditorHandle, {
  siteId: string;
  disabled: boolean;
  directoryChanged: boolean;
  onStateChange: (state: EnvEditorState) => void;
}>(function EnvEditor({ siteId, disabled, directoryChanged, onStateChange }, ref) {
  const t = useT();
  const [view, setView] = React.useState<EnvFileView | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [working, setWorking] = React.useState(false);
  const running = React.useRef(false);
  const generation = React.useRef(0);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [notice, setNotice] = React.useState("");
  const errorRef = React.useRef<HTMLDivElement>(null);
  const [reveal, setReveal] = React.useState<Set<string>>(new Set());
  const [edits, setEdits] = React.useState<Record<string, string>>({});
  const [newKey, setNewKey] = React.useState("");
  const [newValue, setNewValue] = React.useState("");
  const [confirm, setConfirm] = React.useState<"reload" | "discard" | null>(null);
  const keyRef = React.useRef<HTMLInputElement>(null);
  const original = new Map(view?.entries.filter((entry) => !entry.commented).map((entry) => [entry.key, entry]));
  const entries = [...original.values(), ...Object.keys(edits).filter((key) => !original.has(key)).map((key) => ({
    key, value: "", commented: false, secret: isEnvSecretKey(key), line: 0, needsQuote: false,
  }))];
  const pendingInput = !!newKey || !!newValue;
  const dirty = Object.keys(edits).length > 0 || pendingInput;
  const busy = loading || working;
  const blocked = disabled || directoryChanged || busy;
  const keyValid = /^[A-Za-z_][A-Za-z0-9_.]*$/.test(newKey.trim());
  const duplicate = entries.some((entry) => entry.key === newKey.trim());
  const canSave = !!view && Object.keys(edits).length > 0 && !pendingInput && !blocked;
  React.useEffect(() => onStateChange({ dirty, busy, canSave }), [dirty, busy, canSave, onStateChange]);
  React.useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);

  const clearDraft = () => { setEdits({}); setNewKey(""); setNewValue(""); setReveal(new Set()); setNotice(""); };
  const load = React.useCallback(async () => {
    if (running.current) return;
    running.current = true;
    const request = ++generation.current;
    setLoading(true); setError(null); setNotice("");
    try {
      const next = await api.envRead(siteId);
      if (request !== generation.current) return;
      setView(next); setEdits({}); setNewKey(""); setNewValue(""); setReveal(new Set());
    } catch (e) {
      if (request === generation.current) setError(normalizeError(e));
    } finally {
      if (request === generation.current) { running.current = false; setLoading(false); }
    }
  }, [siteId]);
  React.useEffect(() => {
    void load();
    return () => { generation.current++; running.current = false; };
  }, [load]);

  const save = async () => {
    if (!canSave || running.current || !view) return;
    running.current = true; setWorking(true); setError(null); setNotice("");
    const request = generation.current;
    try {
      const next = await api.envSave(siteId, Object.entries(edits), view.revision);
      if (request !== generation.current) return;
      setView(next); clearDraft();
      setNotice(`${t("env.saved")} · ${t(view.exists ? "env.backupHint" : "env.createdHint")}`);
    } catch (e) {
      if (request === generation.current) setError(normalizeError(e));
    } finally {
      if (request === generation.current) { running.current = false; setWorking(false); }
    }
  };
  React.useImperativeHandle(ref, () => ({ save, isBusy: () => running.current }));

  const change = (key: string, value: string) => { setNotice(""); setEdits((previous) => {
    const next = { ...previous, [key]: value };
    if (original.get(key)?.value === value) delete next[key];
    return next;
  }); };
  const applyDb = async () => {
    if (blocked || running.current || !view) return;
    running.current = true; setWorking(true); setError(null); setNotice("");
    const request = generation.current;
    try {
      const values = await api.envPreviewDb(siteId, view.revision);
      if (request !== generation.current) return;
      setEdits((previous) => {
        const next = { ...previous };
        // 用户已经编辑过的同名变量优先，补全不会清空或覆盖现有草稿。
        for (const [key, value] of values) if (!Object.hasOwn(next, key) && original.get(key)?.value !== value) next[key] = value;
        return next;
      });
      setNotice(t("env.dbDraft"));
    } catch (e) {
      if (request === generation.current) setError(normalizeError(e));
    } finally {
      if (request === generation.current) { running.current = false; setWorking(false); }
    }
  };
  const addNew = () => {
    if (!keyValid || duplicate || blocked) return;
    const key = newKey.trim();
    change(key, newValue); setNewKey(""); setNewValue("");
    requestAnimationFrame(() => document.getElementById(`env-value-${key}`)?.focus());
  };
  const reset = (key: string) => { setEdits((previous) => { const next = { ...previous }; delete next[key]; return next; }); keyRef.current?.focus(); };

  return <div className="space-y-4">
    {directoryChanged && <p role="status" className="rounded-xl bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t("env.directoryChanged")}</p>}
    {view && <div className="space-y-3">
      <p className="font-mono text-xs text-secondary [overflow-wrap:anywhere]">{view.path}</p>
      <p className="text-xs leading-relaxed text-secondary">{t("env.saveHint")}</p>
      <div className="flex flex-wrap gap-2">
        {view.dbHint && <Button size="sm" variant="secondary" disabled={blocked} onClick={() => void applyDb()}><Wand2 className="h-3.5 w-3.5" />{t("env.applyDb")}</Button>}
        <Button size="sm" variant="secondary" disabled={blocked} onClick={() => dirty ? setConfirm("reload") : void load()}><RefreshCw className="h-3.5 w-3.5" />{t("env.reload")}</Button>
        {dirty && <Button size="sm" variant="ghost" disabled={disabled || busy} onClick={() => setConfirm("discard")}><Undo2 className="h-3.5 w-3.5" />{t("env.discard")}</Button>}
      </div>
    </div>}
    {error && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-2 rounded-xl bg-error-soft p-3 text-xs leading-relaxed text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]">
      <p>{error.message}</p>{error.hint && <p>{error.hint}</p>}
      {error.detail && <details><summary>{t("sites.detail.deleteErrorDetail")}</summary><p className="mt-2 whitespace-pre-wrap">{error.detail}</p></details>}
      {!view && <Button size="sm" variant="secondary" disabled={busy} onClick={() => void load()}>{t("env.reload")}</Button>}
    </div>}
    {notice && <p role="status" className="rounded-xl bg-fill p-3 text-xs leading-relaxed text-secondary">{notice}</p>}
    {loading ? <div className="space-y-2" aria-busy="true">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-20 w-full" />)}</div> : view && <>
      {entries.length === 0 && <p className="rounded-xl bg-fill p-4 text-sm text-secondary">{t(view.exists ? "env.empty" : "env.notExistsHint")}</p>}
      <div className="space-y-3">{entries.map((entry) => {
        const changed = Object.hasOwn(edits, entry.key);
        const value = changed ? edits[entry.key] : entry.value;
        const masked = entry.secret && !reveal.has(entry.key);
        const id = `env-value-${entry.key}`;
        const multiline = /[\r\n]/.test(value);
        return <div key={entry.key} className={cn("min-w-0 space-y-2 rounded-xl bg-fill p-3", changed && "ring-1 ring-inset ring-primary/35")}>
          <div className="flex items-center justify-between gap-2">
            <Label htmlFor={id} className="min-w-0 font-mono text-xs text-foreground [overflow-wrap:anywhere]">{entry.key}</Label>
            {changed && <Button size="icon-sm" variant="ghost" disabled={blocked} aria-label={`${t("env.undo")} ${entry.key}`} onClick={() => reset(entry.key)}><Undo2 className="h-3.5 w-3.5" /></Button>}
          </div>
          <div className="flex min-w-0 items-start gap-2">
            {multiline && !masked ? <textarea id={id} value={value} disabled={blocked} rows={3} spellCheck={false}
              className="min-w-0 flex-1 resize-y rounded-md border border-border bg-fill p-2 font-mono text-xs text-foreground outline-none focus:border-primary disabled:opacity-50"
              onChange={(event) => change(entry.key, event.target.value)} /> : <Input id={id} type={masked ? "password" : "text"} value={value} readOnly={multiline && masked} disabled={blocked}
              autoComplete="off" spellCheck={false} className="min-w-0 flex-1 font-mono text-xs" onChange={(event) => change(entry.key, event.target.value)} />}
            {entry.secret && <Button size="icon" variant="ghost" disabled={blocked} aria-label={`${t(masked ? "env.showValue" : "env.hideValue")} ${entry.key}`}
              aria-pressed={!masked} onClick={() => setReveal((previous) => { const next = new Set(previous); if (next.has(entry.key)) next.delete(entry.key); else next.add(entry.key); return next; })}>
              {masked ? <Eye className="h-4 w-4" /> : <EyeOff className="h-4 w-4" />}
            </Button>}
          </div>
          {view.entries.filter((other) => !other.commented && other.key === entry.key).length > 1 && <p className="text-xs leading-relaxed text-warn">{t("env.duplicates")}</p>}
          {entry.needsQuote && !changed && <p className="text-xs leading-relaxed text-warn">{t("env.quoteValueHint")}</p>}
          {changed && <p className="text-xs text-secondary">{t(entry.line ? "detail.unsaved" : "env.newDraft")}</p>}
        </div>;
      })}</div>
      <div className="space-y-3 border-t border-dashed border-separator pt-4">
        <div className="space-y-1.5"><Label htmlFor="env-new-key">{t("env.newKey")}</Label>
          <Input ref={keyRef} id="env-new-key" value={newKey} disabled={blocked} spellCheck={false} autoComplete="off" placeholder="APP_NAME"
            aria-invalid={!!newKey && (!keyValid || duplicate)} aria-describedby="env-key-hint" onChange={(event) => setNewKey(event.target.value)} /></div>
        <p id="env-key-hint" className={cn("text-xs leading-relaxed", newKey && (!keyValid || duplicate) ? "text-error" : "text-secondary")}>{t(duplicate ? "env.duplicateKey" : "env.keyHint")}</p>
        <div className="space-y-1.5"><Label htmlFor="env-new-value">{t("env.newValue")}</Label>
          <Input id="env-new-value" value={newValue} disabled={blocked} type={isEnvSecretKey(newKey) ? "password" : "text"} autoComplete="off"
            onChange={(event) => setNewValue(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addNew(); } }} /></div>
        <Button size="sm" variant="secondary" disabled={!keyValid || duplicate || blocked} onClick={addNew}><Plus className="h-3.5 w-3.5" />{t("env.add")}</Button>
        {pendingInput && <p className="text-xs leading-relaxed text-warn">{t("env.addBeforeSave")}</p>}
      </div>
      {view.entries.some((entry) => entry.commented) && <details className="space-y-2 text-xs text-secondary"><summary className="cursor-pointer">{t("env.commented")}</summary>
        <p className="leading-relaxed">{t("env.commentsHint")}</p>
        {view.entries.filter((entry) => entry.commented).map((entry) => <div key={`${entry.key}-${entry.line}`} className="flex items-center justify-between gap-2">
          <span className="min-w-0 font-mono [overflow-wrap:anywhere]">{entry.key}</span>
          <Button size="sm" variant="ghost" disabled={blocked || entries.some((row) => row.key === entry.key)} onClick={() => change(entry.key, entry.value)}>{t("env.useExample")}</Button>
        </div>)}
      </details>}
      {view.variants.length > 0 && <p className="text-xs leading-relaxed text-secondary [overflow-wrap:anywhere]">{t("env.variants")}: {view.variants.join(" · ")}</p>}
    </>}
    <ConfirmDialog open={confirm !== null} onOpenChange={(open) => { if (!open) setConfirm(null); }} title={t("env.discardTitle")}
      description={t(confirm === "reload" ? "env.reloadDiscardHint" : "env.discardHint")} confirmText={t(confirm === "reload" ? "env.reload" : "env.discard")}
      onConfirm={() => { const action = confirm; setConfirm(null); if (action === "reload") void load(); else clearDraft(); }} />
  </div>;
});
