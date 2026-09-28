"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { MongoAuthView } from "@nsb/schema";
import { Eye, EyeOff, KeyRound, RefreshCw, ShieldCheck } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

type Mode = "connect" | "setup" | "enable" | "disable" | "password";
type Draft = { mode: Mode; view: MongoAuthView; signature: string };
const titles = { connect: "mongoAuth.connect", setup: "mongoAuth.setup", enable: "mongoAuth.enable", disable: "mongoAuth.disable", password: "mongoAuth.password" } as const;
const validText = (s: string, max: number) => !!s.trim() && new TextEncoder().encode(s).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(s);

export function MongoAuthPanel({ version, signature, onLockChange }: { version: string; signature: string; onLockChange: (locked: boolean) => void }) {
  const t = useT(); const client = useQueryClient(); const id = React.useId();
  const queryKey = ["mongo-auth", version, signature];
  const [busy, setBusy] = React.useState(false); const lock = React.useRef(false);
  const query = useQuery({ queryKey, queryFn: () => api.mongoAuthStatus(version), enabled: !!version && !busy, retry: (count, error) => count < 2 && normalizeError(error).code === "SERVICE_BUSY", retryDelay: 700, refetchOnWindowFocus: false });
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [username, setUsername] = React.useState(""); const [authDatabase, setAuthDatabase] = React.useState("admin");
  const [password, setPassword] = React.useState(""); const [repeat, setRepeat] = React.useState("");
  const [method, setMethod] = React.useState("password"); const [visible, setVisible] = React.useState(false);
  const [restart, setRestart] = React.useState(false); const [disable, setDisable] = React.useState(false);
  const [error, setError] = React.useState<unknown>(null); const errorRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => { onLockChange(busy || !!draft); return () => onLockChange(false); }, [busy, draft, onLockChange]);
  React.useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);
  const clear = () => { setDraft(null); setPassword(""); setRepeat(""); setError(null); setVisible(false); };
  const open = (mode: Mode) => {
    if (!query.data || lock.current) return;
    setDraft({ mode, view: query.data, signature }); setUsername(mode === "setup" ? "niceenv_admin" : query.data.username);
    setAuthDatabase(mode === "setup" ? "admin" : query.data.authDatabase); setPassword(""); setRepeat("");
    setMethod("password"); setVisible(false); setRestart(false); setDisable(false); setError(null);
  };
  const refreshRelated = () => Promise.all(["mongo-auth", "mongo-overview", "mongo-collections", "mongo-documents", "mongo-backups", "services"].map(key => client.invalidateQueries({ queryKey: [key] })));
  const execute = async (work: () => Promise<void>) => {
    if (lock.current) return; lock.current = true; setBusy(true); setError(null);
    try { await work(); } catch (cause) { setError(cause); }
    finally { lock.current = false; setBusy(false); await refreshRelated(); }
  };
  const refreshDraft = () => void execute(async () => {
    if (!draft) return;
    const view = await api.mongoAuthStatus(version);
    client.setQueryData(queryKey, view);
    setDraft({ mode: draft.mode === "setup" && view.administrator ? "enable" : draft.mode, view, signature }); setRestart(false); setDisable(false);
  });
  const changingPassword = draft?.mode === "setup" || draft?.mode === "password";
  const needsPassword = changingPassword || (draft?.mode === "connect" && method === "password");
  const needsRestart = !!draft && ["setup", "enable", "disable"].includes(draft.mode);
  const credentialsValid = validText(username, 256) && validText(authDatabase, 63) && !/[\s/\\."$*<>:|?]/.test(authDatabase) && validText(password, 4096);
  const canSave = !!draft && !busy && draft.signature === signature && draft.view.running &&
    (draft.mode === "connect" ? method === "none" || credentialsValid : !draft.view.problem) &&
    (!changingPassword || (validText(password, 4096) && Array.from(password).length >= 8 && password === repeat)) &&
    (draft.mode !== "setup" || (credentialsValid && draft.view.hasUsers === false && draft.view.authorization === false)) &&
    (!["enable", "disable", "password"].includes(draft.mode) || draft.view.administrator) &&
    (!needsRestart || restart) && (draft.mode !== "disable" || disable);
  const submit = (event: React.FormEvent) => {
    event.preventDefault(); if (!canSave || !draft) return;
    const current = draft;
    void execute(async () => {
      let result: MongoAuthView;
      if (current.mode === "connect") result = await api.mongoAuthConnection(version, current.view.revision, method === "none" ? { username: "", password: "", authDatabase: "admin" } : { username, password, authDatabase });
      else if (current.mode === "password") result = await api.mongoAuthPassword(version, current.view.revision, password);
      else result = await api.mongoAuthApply(version, { revision: current.view.revision, enabled: current.mode !== "disable", acknowledgeRestart: restart, acknowledgeDisable: disable, administrator: current.mode === "setup" ? { username, password, authDatabase: "admin" } : null });
      client.setQueryData(queryKey, result); clear(); toast.success(t("mongoAuth.saved"));
    });
  };
  const fail = error ? normalizeError(error) : null;
  const view = query.data;
  const stateLabel = (value: boolean | null) => t(value === null ? "mongoAuth.unknown" : value ? "mongoAuth.on" : "mongoAuth.off");
  return <Card>
    <CardHeader><div className="flex flex-wrap items-center justify-between gap-2"><CardTitle className="flex items-center gap-2"><ShieldCheck className="h-4 w-4" />{t("mongoAuth.title")}</CardTitle><Button size="sm" variant="ghost" disabled={busy || query.isFetching} onClick={() => void query.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("mongo.refresh")}</Button></div><CardDescription>{t("mongoAuth.intro")}</CardDescription></CardHeader>
    <CardContent className="min-w-0 space-y-3">
      {!isTauri && <p className="text-xs text-warn">{t("mongoAuth.demo")}</p>}
      {query.isPending && <p role="status" className="text-sm text-muted">{t("mongo.loading")}</p>}
      {query.isError && <p role="alert" className="break-words text-sm text-error">{normalizeError(query.error).message}</p>}
      {view && <>
        <div className="flex flex-wrap gap-x-5 gap-y-2 text-xs text-muted"><p>{t("mongoAuth.runtime")}: {view.running ? stateLabel(view.authorization) : t("mongoAuth.stopped")}</p><p>{t("mongoAuth.configured")}: {stateLabel(view.configured)}</p><p className="break-all">{t("mongoAuth.account")}: {view.hasPassword ? `${view.username} @ ${view.authDatabase}` : t("mongoAuth.noAccount")}</p></div>
        {view.problem && <p role="alert" className="break-words text-sm text-error">{view.problem.message}</p>}
        {view.running && view.authorization !== null && view.authorization !== view.configured && <p role="alert" className="text-xs text-warn">{t("mongoAuth.pending")}</p>}
        {!view.running && <p className="text-xs text-muted">{t("mongoAuth.startFirst")}</p>}
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="secondary" disabled={busy || !view.running} onClick={() => open("connect")}><KeyRound className="h-3.5 w-3.5" />{t("mongoAuth.connect")}</Button>
          {view.hasUsers === false && view.authorization === false ? <Button size="sm" disabled={busy || !view.running || !!view.problem} onClick={() => open("setup")}>{t("mongoAuth.setup")}</Button> : <>
            <Button size="sm" variant="secondary" disabled={busy || !view.running || !view.administrator} onClick={() => open(view.authorization ? "disable" : "enable")}>{t(view.authorization ? "mongoAuth.disable" : "mongoAuth.enable")}</Button>
            <Button size="sm" variant="ghost" disabled={busy || !view.running || !view.administrator} onClick={() => open("password")}>{t("mongoAuth.password")}</Button>
          </>}
        </div>
        {view.hasUsers && !view.administrator && <p className="text-xs leading-5 text-muted">{t("mongoAuth.requireAdmin")}</p>}
      </>}
    </CardContent>
    <Dialog open={!!draft} onOpenChange={open => { if (!open && !lock.current) clear(); }}><DialogContent hideClose={busy} className="flex max-h-[85dvh] max-w-xl flex-col overflow-hidden p-5 sm:p-6" onInteractOutside={e => e.preventDefault()}>
      <DialogHeader className="shrink-0 pr-6"><DialogTitle>{draft && t(titles[draft.mode])}</DialogTitle><DialogDescription>MongoDB {version}</DialogDescription></DialogHeader>
      {draft && <form onSubmit={submit} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
        <p className="text-xs leading-5 text-muted">{t(draft.mode === "connect" ? "mongoAuth.connectionHint" : draft.mode === "password" ? "mongoAuth.passwordHint" : draft.mode === "setup" ? "mongoAuth.setupHint" : "mongoAuth.restartHint")}</p>
        {draft.mode === "connect" && <div className="space-y-1.5"><Label htmlFor={`${id}-method`}>{t("mongoAuth.method")}</Label><Select value={method} disabled={busy} onValueChange={value => { setMethod(value); setPassword(""); }}><SelectTrigger id={`${id}-method`}><SelectValue /></SelectTrigger><SelectContent><SelectItem value="password">{t("mongoAuth.passwordMethod")}</SelectItem><SelectItem value="none">{t("mongoAuth.anonymous")}</SelectItem></SelectContent></Select></div>}
        {(draft.mode === "setup" || (draft.mode === "connect" && method === "password")) && <>
          <div className="space-y-1.5"><Label htmlFor={`${id}-username`}>{t("mongoAuth.username")}</Label><Input id={`${id}-username`} autoComplete="username" value={username} onChange={e => setUsername(e.target.value)} disabled={busy} /></div>
          {draft.mode === "connect" && <div className="space-y-1.5"><Label htmlFor={`${id}-database`}>{t("mongoAuth.authDatabase")}</Label><Input id={`${id}-database`} value={authDatabase} onChange={e => setAuthDatabase(e.target.value)} disabled={busy} /><p className="text-xs text-muted">{t("mongoAuth.databaseHint")}</p></div>}
        </>}
        {needsPassword && <div className="space-y-1.5"><Label htmlFor={`${id}-password`}>{t(changingPassword ? "mongoAuth.newPassword" : "mongoAuth.loginPassword")}</Label><div className="flex gap-2"><Input id={`${id}-password`} className="min-w-0" type={visible ? "text" : "password"} autoComplete={changingPassword ? "new-password" : "current-password"} value={password} onChange={e => setPassword(e.target.value)} disabled={busy} /><Button type="button" size="icon" variant="ghost" aria-label={t(visible ? "mongoAuth.hide" : "mongoAuth.show")} disabled={busy} onClick={() => setVisible(!visible)}>{visible ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}</Button></div>{changingPassword && <p className="text-xs text-muted">{t("mongoAuth.minimum")}</p>}</div>}
        {changingPassword && <div className="space-y-1.5"><Label htmlFor={`${id}-repeat`}>{t("mongoAuth.repeat")}</Label><Input id={`${id}-repeat`} type={visible ? "text" : "password"} autoComplete="new-password" value={repeat} onChange={e => setRepeat(e.target.value)} disabled={busy} />{repeat && repeat !== password && <p role="alert" className="text-xs text-error">{t("mongoAuth.mismatch")}</p>}</div>}
        {draft.mode === "disable" && <label className="flex items-start gap-3 text-sm leading-6"><Switch checked={disable} disabled={busy} onCheckedChange={setDisable} /><span>{t("mongoAuth.disableConfirm")}</span></label>}
        {needsRestart && <label className="flex items-start gap-3 text-sm leading-6"><Switch checked={restart} disabled={busy} onCheckedChange={setRestart} /><span>{t("mongoAuth.restartConfirm")}</span></label>}
        {draft.signature !== signature && !busy && <p role="alert" className="text-sm text-warn">{t("mongoAuth.changed")}</p>}
        {!!draft.view.problem && draft.mode !== "connect" && <p role="alert" className="text-sm text-error">{draft.view.problem.message}</p>}
        {fail && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-2 break-words text-sm text-error"><p>{fail.message}</p>{fail.hint && <p className="text-xs leading-5">{fail.hint}</p>}</div>}
      </div><DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-border pt-4"><Button type="button" variant="ghost" disabled={busy} onClick={clear}>{t("common.cancel")}</Button>{(!!error || draft.signature !== signature) && <Button type="button" variant="secondary" disabled={busy} onClick={refreshDraft}>{t("mongoAuth.recheck")}</Button>}<Button type="submit" variant={draft.mode === "disable" ? "destructive" : "default"} disabled={!canSave}>{busy ? t("confirm.busy") : t(needsRestart ? "mongoAuth.applyRestart" : draft.mode === "connect" ? "mongoAuth.verifySave" : "common.save")}</Button></DialogFooter></form>}
    </DialogContent></Dialog>
  </Card>;
}
