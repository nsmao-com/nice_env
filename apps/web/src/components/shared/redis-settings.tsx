"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2, Plus, Settings2, Trash2 } from "lucide-react";
import type { ConfigFileInfo, RedisSettings, RedisSettingsView } from "@nsb/schema";
import * as api from "@/lib/api";
import { useT } from "@/lib/store";
import { useInvalidate } from "@/lib/hooks";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { ConfirmDialog } from "./misc";
import { ConfigEditDialog } from "./config-editor";

const POLICIES = ["noeviction", "allkeys-lru", "allkeys-lfu", "allkeys-random", "volatile-lru", "volatile-lfu", "volatile-random", "volatile-ttl"] as const;
const UNITS = { B: 1, KiB: 1024, MiB: 1024 ** 2, GiB: 1024 ** 3 };
type Draft = { memoryMode: string; memorySize: string; memoryUnit: keyof typeof UNITS; policy: string; timeout: string; clients: string; snapshotMode: string; rules: { seconds: string; changes: string }[]; fsync: string };

export function redisSettingsDraft(settings: RedisSettings): Draft {
  const memory = settings.maxMemoryBytes;
  const unit = (["GiB", "MiB", "KiB", "B"] as const).find((unit) => memory && memory % UNITS[unit] === 0) ?? "MiB";
  return { memoryMode: memory === null ? "default" : memory === 0 ? "unlimited" : "limited", memorySize: memory ? String(memory / UNITS[unit]) : "256", memoryUnit: unit,
    policy: settings.evictionPolicy ?? "default", timeout: settings.timeoutSeconds?.toString() ?? "", clients: settings.maxClients?.toString() ?? "",
    snapshotMode: settings.saveRules === null ? "default" : settings.saveRules.length ? "custom" : "off",
    rules: (settings.saveRules?.length ? settings.saveRules : [{ seconds: 900, changes: 1 }]).map((r) => ({ seconds: String(r.seconds), changes: String(r.changes) })), fsync: settings.appendFsync ?? "default" };
}

export function redisSettingsInput(draft: Draft): { settings: RedisSettings | null; problem: "memory" | "connections" | "snapshots" | null } {
  const integer = (value: string, min: number, max = 2147483647) => /^\d+$/.test(value) && Number.isSafeInteger(Number(value)) && Number(value) >= min && Number(value) <= max;
  const bytes = Number(draft.memorySize) * UNITS[draft.memoryUnit];
  if (draft.memoryMode === "limited" && (!integer(draft.memorySize, 1, Number.MAX_SAFE_INTEGER) || !Number.isSafeInteger(bytes))) return { settings: null, problem: "memory" };
  if ((draft.timeout !== "" && !integer(draft.timeout, 0)) || (draft.clients !== "" && !integer(draft.clients, 1))) return { settings: null, problem: "connections" };
  if (draft.snapshotMode === "custom" && (!draft.rules.length || draft.rules.length > 16 || draft.rules.some((r) => !integer(r.seconds, 1) || !integer(r.changes, 0)))) return { settings: null, problem: "snapshots" };
  return { settings: { maxMemoryBytes: draft.memoryMode === "default" ? null : draft.memoryMode === "unlimited" ? 0 : bytes, evictionPolicy: draft.policy === "default" ? null : draft.policy,
    timeoutSeconds: draft.timeout === "" ? null : Number(draft.timeout), maxClients: draft.clients === "" ? null : Number(draft.clients),
    saveRules: draft.snapshotMode === "default" ? null : draft.snapshotMode === "off" ? [] : draft.rules.map((r) => ({ seconds: Number(r.seconds), changes: Number(r.changes) })), appendFsync: draft.fsync === "default" ? null : draft.fsync }, problem: null };
}

export function RedisSettingsButton({ version }: { version?: string | null }) {
  const t = useT();
  const [target, setTarget] = React.useState<string | null>(null);
  return <><Button variant="secondary" size="sm" disabled={!version} onClick={() => setTarget(version!)}><Settings2 className="size-3.5" />{t("redisSettings.title")}</Button>
    {target && <RedisSettingsDialog key={target} version={target} onClose={() => setTarget(null)} />}</>;
}

function RedisSettingsDialog({ version, onClose }: { version: string; onClose: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: ["redis-settings", version], queryFn: () => api.redisSettings(version), retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const [baseline, setBaseline] = React.useState<RedisSettingsView | null>(null);
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [tab, setTab] = React.useState("memory");
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const errorRef = React.useRef<HTMLDivElement>(null);
  const [saved, setSaved] = React.useState(false);
  const [acknowledge, setAcknowledge] = React.useState(false);
  const [confirm, setConfirm] = React.useState<"close" | "reload" | "advanced" | null>(null);
  const [advanced, setAdvanced] = React.useState<ConfigFileInfo | null>(null);
  React.useEffect(() => { if (!baseline && query.data && !query.isFetching && !query.isError) { setBaseline(query.data); setDraft(redisSettingsDraft(query.data.settings)); } }, [baseline, query.data, query.isFetching, query.isError]);
  React.useEffect(() => { if (error || query.isError) errorRef.current?.focus(); }, [error, query.isError, query.error]);
  const parsed = draft ? redisSettingsInput(draft) : null;
  const dirty = !!draft && !!baseline && (parsed?.settings ? JSON.stringify(parsed.settings) !== JSON.stringify(baseline.settings) : JSON.stringify(draft) !== JSON.stringify(redisSettingsDraft(baseline.settings)));
  const disabling = draft?.snapshotMode === "off" && baseline?.settings.saveRules?.length !== 0;
  const disabled = busy || query.isFetching;
  const update = (patch: Partial<Draft>) => { setDraft((old) => old ? { ...old, ...patch } : old); setSaved(false); setAcknowledge(false); };
  const reload = async () => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { const result = await query.refetch({ cancelRefetch: false }); if (result.error) throw result.error;
      if (result.data) { setBaseline(result.data); setDraft(redisSettingsDraft(result.data.settings)); setAcknowledge(false); setSaved(false); }
    } catch (e) { setError(normalizeError(e)); } finally { busyRef.current = false; setBusy(false); }
  };
  const openAdvanced = async () => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(null);
    try { const file = (await api.configList()).find((file) => file.kind === `redis-conf@${version}`);
      if (!file?.exists) throw { code: "CONFIG_NOT_GENERATED", message: t("redisSettings.missing") };
      setAdvanced(file);
    } catch (e) { setError(normalizeError(e)); } finally { busyRef.current = false; setBusy(false); }
  };
  const request = (action: "close" | "reload" | "advanced") => { if (busyRef.current) return; if (dirty) setConfirm(action); else if (action === "close") onClose(); else if (action === "reload") void reload(); else void openAdvanced(); };
  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    if (busyRef.current || disabled || !baseline || !parsed?.settings || !dirty || (disabling && !acknowledge)) return;
    busyRef.current = true; setBusy(true); setError(null); setSaved(false);
    try { const view = await api.redisSettingsSave(version, baseline.revision, parsed.settings, acknowledge);
      queryClient.setQueryData(["redis-settings", version], view);
      setBaseline(view); setDraft(redisSettingsDraft(view.settings)); setSaved(true); setAcknowledge(false); invalidate("config-files", "config-backups", "backups");
    } catch (e) { setError(normalizeError(e)); } finally { busyRef.current = false; setBusy(false); }
  };
  const choice = (id: string, value: string, onChange: (value: string) => void, options: { value: string; label: string }[]) => <Select value={value} onValueChange={onChange} disabled={disabled}><SelectTrigger id={id} className="w-full"><SelectValue /></SelectTrigger><SelectContent>{options.map((o) => <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>)}</SelectContent></Select>;
  const defaults = { value: "default", label: t("redisSettings.default") };
  const failure = error ?? (query.isError ? normalizeError(query.error) : null);
  return <>
    <Dialog open={!advanced} onOpenChange={(open) => !open && request("close")}>
      <DialogContent hideClose={busy} className="flex max-h-[90dvh] max-w-2xl flex-col overflow-hidden p-4 sm:p-6">
        <DialogHeader className="shrink-0 pr-6"><DialogTitle className="leading-snug">Redis {version} · {t("redisSettings.title")}</DialogTitle><DialogDescription>{t("redisSettings.intro")}</DialogDescription></DialogHeader>
        <form onSubmit={save} className="flex min-h-0 flex-1 flex-col gap-4">
          <div className="min-h-0 space-y-4 overflow-y-auto px-0.5 text-xs leading-relaxed">
            {!isTauri && <p className="text-warn">{t("redisSettings.demo")}</p>}
            {query.isFetching && <p role="status" className="text-muted">{t("common.loading")}</p>}
            {failure && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-1 rounded-lg bg-error-soft p-3 text-error outline-none [overflow-wrap:anywhere]"><p>{failure.message}</p>{failure.hint && <p>{failure.hint}</p>}</div>}
            {saved && <p role="status" className="rounded-lg bg-fill p-3 text-secondary">{t("redisSettings.saved")}</p>}
            {draft && baseline && <Tabs value={tab} onValueChange={setTab}>
              <TabsList className="mb-4 max-w-full"><TabsTrigger value="memory" disabled={disabled}>{t("redisSettings.memoryTab")}{parsed?.problem && parsed.problem !== "snapshots" ? " •" : ""}</TabsTrigger><TabsTrigger value="persistence" disabled={disabled}>{t("redisSettings.persistenceTab")}{parsed?.problem === "snapshots" ? " •" : ""}</TabsTrigger></TabsList>
              {parsed?.problem && <p role="alert" className="mb-3 text-error">{t(`redisSettings.invalid.${parsed.problem}`)}</p>}
              <TabsContent value="memory" className="mt-0 space-y-5">
                <div className="space-y-2"><Label htmlFor="redis-memory-mode">{t("redisSettings.maxMemory")}</Label>
                  {choice("redis-memory-mode", draft.memoryMode, (memoryMode) => update({ memoryMode }), [defaults, { value: "unlimited", label: t("redisSettings.unlimited") }, { value: "limited", label: t("redisSettings.limited") }])}
                  {draft.memoryMode === "limited" && <div className="grid grid-cols-[minmax(0,1fr)_6rem] gap-2"><Input type="number" aria-label={t("redisSettings.memorySize")} min={1} step={1} value={draft.memorySize} disabled={disabled} onChange={(e) => update({ memorySize: e.target.value })} /><Label htmlFor="redis-memory-unit" className="sr-only">{t("redisSettings.memoryUnit")}</Label>{choice("redis-memory-unit", draft.memoryUnit, (value) => update({ memoryUnit: value as keyof typeof UNITS }), Object.keys(UNITS).map((value) => ({ value, label: value })))}</div>}
                  <p className="text-muted">{t("redisSettings.memoryHint")}</p>
                </div>
                <div className="space-y-2"><Label htmlFor="redis-policy">{t("redisSettings.policy")}</Label>{choice("redis-policy", draft.policy, (policy) => update({ policy }), [defaults, ...(!POLICIES.includes(draft.policy as typeof POLICIES[number]) && draft.policy !== "default" ? [{ value: draft.policy, label: draft.policy }] : []), ...POLICIES.map((value) => ({ value, label: t(`redisSettings.policy.${value}`) }))])}<p className="text-muted">{t("redisSettings.policyHint")}</p></div>
                <div className="grid grid-cols-1 gap-4 border-t border-dashed border-separator pt-4 sm:grid-cols-2">
                  <div className="space-y-1.5"><Label htmlFor="redis-timeout">{t("redisSettings.timeout")}</Label><Input id="redis-timeout" type="number" min={0} step={1} value={draft.timeout} disabled={disabled} placeholder={t("redisSettings.default")} onChange={(e) => update({ timeout: e.target.value })} /><p className="text-muted">{t("redisSettings.timeoutHint")}</p></div>
                  <div className="space-y-1.5"><Label htmlFor="redis-clients">{t("redisSettings.clients")}</Label><Input id="redis-clients" type="number" min={1} step={1} value={draft.clients} disabled={disabled} placeholder={t("redisSettings.default")} onChange={(e) => update({ clients: e.target.value })} /><p className="text-muted">{t("redisSettings.clientsHint")}</p></div>
                </div>
              </TabsContent>
              <TabsContent value="persistence" className="mt-0 space-y-5">
                <div className="space-y-2"><Label htmlFor="redis-snapshots">{t("redisSettings.snapshots")}</Label>{choice("redis-snapshots", draft.snapshotMode, (snapshotMode) => update({ snapshotMode }), [defaults, { value: "custom", label: t("redisSettings.custom") }, { value: "off", label: t("redisSettings.off") }])}<p className="text-muted">{t("redisSettings.snapshotHint")}</p></div>
                {draft.snapshotMode === "custom" && <div className="space-y-3">
                  {draft.rules.map((rule, index) => <div key={index} className="space-y-2 rounded-lg bg-fill p-3">
                    <div className="flex items-center justify-between gap-2"><span className="font-medium">{t("redisSettings.rule").replace("{n}", String(index + 1))}</span><Button type="button" size="icon-sm" variant="ghost" title={t("redisSettings.removeRule").replace("{n}", String(index + 1))} disabled={disabled || draft.rules.length <= 1} onClick={() => update({ rules: draft.rules.filter((_, i) => i !== index) })}><Trash2 className="size-3.5" /></Button></div>
                    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                      <div className="space-y-1.5"><Label htmlFor={`redis-seconds-${index}`}>{t("redisSettings.seconds")}</Label><Input id={`redis-seconds-${index}`} type="number" min={1} step={1} value={rule.seconds} disabled={disabled} onChange={(e) => update({ rules: draft.rules.map((r, i) => i === index ? { ...r, seconds: e.target.value } : r) })} /></div>
                      <div className="space-y-1.5"><Label htmlFor={`redis-changes-${index}`}>{t("redisSettings.changes")}</Label><Input id={`redis-changes-${index}`} type="number" min={0} step={1} value={rule.changes} disabled={disabled} onChange={(e) => update({ rules: draft.rules.map((r, i) => i === index ? { ...r, changes: e.target.value } : r) })} /></div>
                    </div>
                  </div>)}
                  <Button type="button" variant="ghost" size="sm" disabled={disabled || draft.rules.length >= 16} onClick={() => update({ rules: [...draft.rules, { seconds: "300", changes: "10" }] })}><Plus className="size-3.5" />{t("redisSettings.addRule")}</Button>
                </div>}
                {disabling && <label className="flex items-start gap-2 rounded-lg bg-warn-soft p-3 text-warn"><input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={acknowledge} disabled={disabled} onChange={(e) => setAcknowledge(e.target.checked)} /><span>{t("redisSettings.confirmDisable")}</span></label>}
                <div className="space-y-2 border-t border-dashed border-separator pt-4"><p className="font-medium">{t("redisSettings.aof")} · {t(baseline.appendOnly === null ? "redisSettings.default" : baseline.appendOnly ? "redisSettings.aofOn" : "redisSettings.aofOff")}</p><p className="text-muted">{t("redisSettings.aofHint")}</p>
                  {baseline.appendOnly && <><Label htmlFor="redis-fsync">{t("redisSettings.fsync")}</Label>{choice("redis-fsync", draft.fsync, (fsync) => update({ fsync }), [defaults, ...(!["default", "always", "everysec", "no"].includes(draft.fsync) ? [{ value: draft.fsync, label: draft.fsync }] : []), ...(["always", "everysec", "no"] as const).map((value) => ({ value, label: t(`redisSettings.fsync.${value}`) }))])}</>}
                </div>
              </TabsContent>
            </Tabs>}
            <div className="space-y-2 border-t border-dashed border-separator pt-3"><p className="text-muted">{t("redisSettings.scope")}</p>{baseline && <p className="font-mono text-faint [overflow-wrap:anywhere]">{baseline.path}</p>}<Button type="button" size="sm" variant="ghost" disabled={disabled} onClick={() => request("advanced")}>{t("redisSettings.advanced")}</Button></div>
          </div>
          <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-3">
            <Button type="button" size="sm" variant="ghost" disabled={disabled} onClick={() => request("reload")}>{t("redisSettings.reload")}</Button>
            <Button type="button" size="sm" variant="ghost" disabled={busy} onClick={() => request("close")}>{t("common.close")}</Button>
            <Button type="submit" size="sm" disabled={disabled || !dirty || !parsed?.settings || (disabling && !acknowledge)}>{busy && <Loader2 className="size-3.5 animate-spin motion-reduce:animate-none" />}{t("redisSettings.save")}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={!!confirm} onOpenChange={(open) => !open && setConfirm(null)} title={t("redisSettings.discardTitle")} description={t("redisSettings.discardHint")} confirmText={t("redisSettings.discard")}
      onConfirm={() => { const action = confirm; setConfirm(null); if (action === "close") onClose(); else if (action === "reload") void reload(); else void openAdvanced(); }} />
    {advanced && <ConfigEditDialog info={advanced} onClose={() => { setAdvanced(null); void reload(); }} onSaved={() => invalidate("redis-settings", "config-files", "backups")} />}
  </>;
}
