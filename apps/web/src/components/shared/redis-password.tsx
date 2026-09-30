"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Loader2 } from "lucide-react";
import type { ConfigFileInfo, RedisPasswordView } from "@nsb/schema";
import * as api from "@/lib/api";
import { useT } from "@/lib/store";
import { useInvalidate } from "@/lib/hooks";
import { sameVersion } from "@/lib/utils";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { ConfirmDialog } from "./misc";
import { ConfigEditDialog } from "./config-editor";

export function RedisPasswordButton({ version, running }: { version?: string | null; running?: boolean }) {
  const t = useT();
  const [target, setTarget] = React.useState<string | null>(null);
  return <><Button variant="secondary" size="sm" disabled={!version} onClick={() => setTarget(version!)}><KeyRound className="size-3.5" />{t("redisPassword.title")}</Button>
    {target && <RedisPasswordDialog key={target} version={target} running={!!running} changed={!sameVersion(target, version)} onClose={() => setTarget(null)} />}</>;
}

function RedisPasswordDialog({ version, running, changed, onClose }: { version: string; running: boolean; changed: boolean; onClose: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["redis-password", version], queryFn: () => api.redisPassword(version), retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const [baseline, setBaseline] = React.useState<RedisPasswordView | null>(null);
  const [mode, setMode] = React.useState("password");
  const [password, setPassword] = React.useState("");
  const [confirmation, setConfirmation] = React.useState("");
  const [visible, setVisible] = React.useState(false);
  const [acknowledge, setAcknowledge] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [saved, setSaved] = React.useState(false);
  const [partial, setPartial] = React.useState(false);
  const [confirm, setConfirm] = React.useState<"close" | "reload" | "advanced" | "stop" | null>(null);
  const [advanced, setAdvanced] = React.useState<ConfigFileInfo | null>(null);
  const errorRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => { if (!baseline && query.data && !query.isFetching && !query.isError) setBaseline(query.data); }, [baseline, query.data, query.isFetching, query.isError]);
  React.useEffect(() => { if (error || query.isError || partial) errorRef.current?.focus(); }, [error, query.isError, partial]);
  const dirty = !!password || !!confirmation || (mode === "none" && !!baseline?.enabled);
  const valid = !!password.trim() && new TextEncoder().encode(password).length <= 512 && !/[\x00-\x1f\x7f-\x9f]/.test(password);
  const disabled = busy || query.isFetching;
  const reset = () => { setPassword(""); setConfirmation(""); setVisible(false); setAcknowledge(false); setSaved(false); setPartial(false); };
  const perform = async (work: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { await work(); } catch (cause) { setError(normalizeError(cause)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  const reload = () => perform(async () => {
    const result = await query.refetch({ cancelRefetch: false });
    if (result.error) throw result.error;
    if (result.data) { setBaseline(result.data); reset(); setMode("password"); }
  });
  const openAdvanced = () => perform(async () => {
    const file = (await api.configList()).find((file) => file.kind === `redis-conf@${version}`);
    if (!file?.exists) throw { message: t("redisSettings.missing") };
    reset(); setAdvanced(file);
  });
  const request = (action: "close" | "reload" | "advanced") => {
    if (busyRef.current) return;
    if (dirty || partial) setConfirm(action);
    else if (action === "close") onClose(); else if (action === "reload") void reload(); else void openAdvanced();
  };
  const stop = () => perform(async () => {
    try { await api.redisPasswordStop(version); setConfirm(null); }
    finally { invalidate("services", "redis-stats"); }
  });
  const canSave = !disabled && !running && !changed && !!baseline && !baseline.blockedReason && !query.isError &&
    (mode === "none" ? acknowledge && (baseline.enabled || partial) : valid && password === confirmation);
  const submit = (event: React.FormEvent) => {
    event.preventDefault(); if (!canSave || !baseline) return;
    void perform(async () => {
      const result = await api.redisPasswordSave(version, baseline.revision, mode === "none" ? "" : password, acknowledge);
      setBaseline(result.view); client.setQueryData(["redis-password", version], result.view);
      invalidate("redis-settings", "redis-connection", "config-files", "config-backups", "backups");
      if (result.connectionSaved) { reset(); setSaved(true); }
      else { setSaved(false); setPartial(true); }
    });
  };
  const failure = error ?? (query.isError ? normalizeError(query.error) : null);
  return <>
    <Dialog open={!advanced} onOpenChange={(open) => !open && request("close")}>
      <DialogContent hideClose={busy} className="flex max-h-[90dvh] max-w-lg flex-col overflow-hidden p-4 sm:p-6">
        <DialogHeader className="shrink-0 pr-6"><DialogTitle className="leading-snug">Redis {version} · {t("redisPassword.title")}</DialogTitle><DialogDescription>{t("redisPassword.intro")}</DialogDescription></DialogHeader>
        <form onSubmit={submit} className="flex min-h-0 flex-col gap-4">
          <div className="min-h-0 space-y-4 overflow-y-auto px-0.5 text-xs leading-relaxed">
            {!isTauri && <p className="text-warn">{t("redisSettings.demo")}</p>}
            {query.isFetching && <p role="status">{t("common.loading")}</p>}
            {failure && confirm !== "stop" && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-1 rounded-lg bg-error-soft p-3 text-error outline-none [overflow-wrap:anywhere]"><p>{failure.message}</p>{failure.hint && <p>{failure.hint}</p>}</div>}
            {changed && <p role="alert" className="text-error">{t("db.pgChanged")}</p>}
            {running && <div className="space-y-2 rounded-lg bg-warn-soft p-3 text-warn"><p role="status">{t("redisPassword.running")}</p><Button type="button" variant="secondary" size="sm" disabled={disabled || changed} onClick={() => { setError(null); setConfirm("stop"); }}>{t("redisPassword.stop")}</Button></div>}
            {saved && <p role="status" className="rounded-lg bg-fill p-3 text-secondary">{t("redisPassword.saved")}</p>}
            {partial && <div ref={errorRef} tabIndex={-1} role="alert" className="rounded-lg bg-warn-soft p-3 text-warn outline-none">{t("redisPassword.partial")}</div>}
            {baseline && (baseline.blockedReason ? <p role="alert" className="rounded-lg bg-warn-soft p-3 text-warn [overflow-wrap:anywhere]">{baseline.blockedReason}</p> : <>
              <p className="text-muted">{t(baseline.enabled ? "redisPassword.enabled" : "redisPassword.disabled")}</p>
              <div className="space-y-2"><Label htmlFor="redis-password-mode">{t("db.operation")}</Label><Select value={mode} disabled={disabled} onValueChange={(value) => { setMode(value); setSaved(false); setAcknowledge(false); }}><SelectTrigger id="redis-password-mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="password">{t("redisPassword.set")}</SelectItem><SelectItem value="none">{t("redisPassword.remove")}</SelectItem></SelectContent></Select></div>
              {mode === "password" ? <div className="space-y-4 border-t border-dashed border-separator pt-4">
                <div className="space-y-1.5"><Label htmlFor="redis-service-password">{t("dbPassword.newPassword")}</Label><Input id="redis-service-password" type={visible ? "text" : "password"} autoComplete="new-password" value={password} maxLength={512} disabled={disabled} aria-invalid={!!password && !valid} aria-describedby="redis-password-help" onChange={(event) => { setPassword(event.target.value); setSaved(false); }} /><p id="redis-password-help" className={password && !valid ? "text-error" : "text-muted"}>{t("redisPassword.passwordHint")}</p></div>
                <div className="space-y-1.5"><Label htmlFor="redis-service-password-confirm">{t("dbPassword.confirmPassword")}</Label><Input id="redis-service-password-confirm" type={visible ? "text" : "password"} autoComplete="new-password" value={confirmation} maxLength={512} disabled={disabled} aria-invalid={!!confirmation && confirmation !== password} onChange={(event) => setConfirmation(event.target.value)} />{confirmation && confirmation !== password && <p role="alert" className="text-error">{t("dbPassword.mismatch")}</p>}</div>
                <label className="flex items-center gap-2"><input type="checkbox" className="size-4 accent-primary" checked={visible} disabled={disabled} onChange={(event) => setVisible(event.target.checked)} />{t("dbPassword.show")}</label>
              </div> : <label className="flex items-start gap-2 rounded-lg bg-warn-soft p-3 text-warn"><input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={acknowledge} disabled={disabled} onChange={(event) => setAcknowledge(event.target.checked)} /><span>{t("redisPassword.acknowledge")}</span></label>}
              <p className="text-muted">{t("redisPassword.clients")}</p>
            </>)}
          </div>
          <DialogFooter className="shrink-0 flex-wrap gap-2 border-t border-dashed border-separator pt-4">
            <Button type="button" variant="ghost" disabled={disabled || changed} onClick={() => request("reload")}>{t("redisPassword.reload")}</Button>
            {baseline?.blockedReason && <Button type="button" variant="secondary" disabled={disabled || changed} onClick={() => request("advanced")}>{t("redisSettings.advanced")}</Button>}
            <Button type="button" variant="ghost" disabled={busy} onClick={() => request("close")}>{t("common.close")}</Button>
            {!baseline?.blockedReason && <Button type="submit" disabled={!canSave}>{busy && <Loader2 className="size-3.5 animate-spin motion-reduce:animate-none" />}{t("redisPassword.save")}</Button>}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={!!confirm && confirm !== "stop"} onOpenChange={(open) => !open && setConfirm(null)} title={t("redisSettings.discardTitle")} description={t("redisSettings.discardHint")} confirmText={t("redisSettings.discard")} onConfirm={() => { const action = confirm; setConfirm(null); if (action === "close") onClose(); else if (action === "reload") void reload(); else if (action === "advanced") void openAdvanced(); }} />
    <ConfirmDialog open={confirm === "stop"} onOpenChange={(open) => { if (!open && !busyRef.current) { setConfirm(null); setError(null); } }} title={t("redisPassword.stopTitle").replace("{version}", version)} description={t("redisPassword.stopHint")} confirmText={t("common.stop")} loading={busy} confirmDisabled={changed || !running} onConfirm={() => { if (!changed && running) void stop(); }}>
      {failure && <div ref={errorRef} role="alert" tabIndex={-1} className="space-y-1 text-xs text-error outline-none [overflow-wrap:anywhere]"><p>{failure.message}</p>{failure.hint && <p>{failure.hint}</p>}</div>}
    </ConfirmDialog>
    {advanced && <ConfigEditDialog info={advanced} onClose={() => { setAdvanced(null); void reload(); }} onSaved={() => invalidate("redis-password", "redis-settings", "config-files", "backups")} />}
  </>;
}
