"use client";

import * as React from "react";
import { Loader2, Settings2 } from "lucide-react";
import type { MemcachedSettings, MemcachedSettingsView } from "@nsb/schema";
import * as api from "@/lib/api";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
import { useT } from "@/lib/store";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";

type Draft = { memoryMb: string; maxConnections: string; threads: string };

function toDraft(settings: MemcachedSettings): Draft {
  return { memoryMb: String(settings.memoryMb), maxConnections: String(settings.maxConnections), threads: String(settings.threads) };
}

function fromDraft(draft: Draft): MemcachedSettings | null {
  const values = { memoryMb: Number(draft.memoryMb), maxConnections: Number(draft.maxConnections), threads: Number(draft.threads) };
  if (!Number.isInteger(values.memoryMb) || values.memoryMb < 64 || values.memoryMb > 1_048_576) return null;
  if (!Number.isInteger(values.maxConnections) || values.maxConnections < 16 || values.maxConnections > 1_000_000) return null;
  if (!Number.isInteger(values.threads) || values.threads < 1 || values.threads > 64) return null;
  return values;
}

export function MemcachedSettingsButton({ version }: { version?: string | null }) {
  const t = useT();
  const [target, setTarget] = React.useState<string | null>(null);
  return <>
    <Button variant="secondary" size="sm" disabled={!version} onClick={() => setTarget(version!)}>
      <Settings2 className="size-3.5" />{t("memcachedSettings.title")}
    </Button>
    {target && <MemcachedSettingsDialog key={target} version={target} onClose={() => setTarget(null)} />}
  </>;
}

function MemcachedSettingsDialog({ version, onClose }: { version: string; onClose: () => void }) {
  const t = useT();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: ["memcached-settings", version], queryFn: () => api.memcachedSettings(version), retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const [baseline, setBaseline] = React.useState<MemcachedSettingsView | null>(null);
  const [draft, setDraft] = React.useState<Draft | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const busyRef = React.useRef(false);
  React.useEffect(() => {
    if (!baseline && query.data && !query.isFetching && !query.isError) {
      setBaseline(query.data);
      setDraft(toDraft(query.data.settings));
    }
  }, [baseline, query.data, query.isFetching, query.isError]);
  const parsed = draft ? fromDraft(draft) : null;
  const dirty = !!draft && !!baseline && (!!parsed ? JSON.stringify(parsed) !== JSON.stringify(baseline.settings) : true);
  const set = (patch: Partial<Draft>) => { setDraft((old) => old ? { ...old, ...patch } : old); setError(null); };
  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    if (busyRef.current || !baseline || !parsed || !dirty) return;
    busyRef.current = true; setBusy(true); setError(null);
    try {
      const next = await api.memcachedSettingsSave(version, baseline.revision, parsed);
      queryClient.setQueryData(["memcached-settings", version], next);
      toast.success(t("memcachedSettings.saved"));
      onClose();
    } catch (cause) {
      setError(normalizeError(cause));
    } finally {
      busyRef.current = false; setBusy(false);
    }
  };
  const reload = async () => {
    if (busyRef.current) return;
    setError(null); setBusy(true); busyRef.current = true;
    try {
      const result = await query.refetch({ cancelRefetch: false });
      if (result.data) { setBaseline(result.data); setDraft(toDraft(result.data.settings)); }
    } catch (cause) { setError(normalizeError(cause)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  const close = () => { if (!busyRef.current) onClose(); };
  return <Dialog open onOpenChange={(open) => !open && close()}>
    <DialogContent hideClose={busy} className="max-w-lg">
      <DialogHeader>
        <DialogTitle>{t("memcachedSettings.title")}</DialogTitle>
        <DialogDescription>{t("memcachedSettings.description")}</DialogDescription>
      </DialogHeader>
      {query.isPending && <p role="status" className="text-sm text-muted">{t("common.loading")}</p>}
      {query.isError && <div role="alert" className="space-y-2 rounded-lg bg-error-soft p-3 text-sm text-error"><p>{normalizeError(query.error).message}</p><Button type="button" size="sm" variant="secondary" onClick={() => void reload()} disabled={busy}>{t("common.retry")}</Button></div>}
      {draft && <form className="space-y-4" onSubmit={(event) => void save(event)}>
        <div className="space-y-1.5"><Label htmlFor="memcached-memory">{t("memcachedSettings.memory")}</Label><Input id="memcached-memory" type="number" min={64} max={1048576} step={1} inputMode="numeric" value={draft.memoryMb} disabled={busy} onChange={(event) => set({ memoryMb: event.target.value })} /><p className="text-xs leading-5 text-muted">{t("memcachedSettings.memoryHint")}</p></div>
        <div className="space-y-1.5"><Label htmlFor="memcached-connections">{t("memcachedSettings.connections")}</Label><Input id="memcached-connections" type="number" min={16} max={1000000} step={1} inputMode="numeric" value={draft.maxConnections} disabled={busy} onChange={(event) => set({ maxConnections: event.target.value })} /></div>
        <div className="space-y-1.5"><Label htmlFor="memcached-threads">{t("memcachedSettings.threads")}</Label><Input id="memcached-threads" type="number" min={1} max={64} step={1} inputMode="numeric" value={draft.threads} disabled={busy} onChange={(event) => set({ threads: event.target.value })} /><p className="text-xs leading-5 text-muted">{t("memcachedSettings.threadsHint")}</p></div>
        <p className="rounded-lg bg-warn-soft p-3 text-xs leading-5 text-warn">{t("memcachedSettings.restartHint")}</p>
        {draft && !parsed && <p role="alert" className="text-xs text-error">{t("memcachedSettings.invalid")}</p>}
        {error && <div role="alert" className="space-y-1 rounded-lg bg-error-soft p-3 text-xs text-error"><p>{error.message}</p>{error.hint && <p>{error.hint}</p>}{error.code === "CONFIG_CONFLICT" && <Button type="button" size="sm" variant="ghost" onClick={() => void reload()} disabled={busy}>{t("common.retry")}</Button>}</div>}
        <DialogFooter><Button type="button" variant="ghost" onClick={close} disabled={busy}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !parsed || !dirty}>{busy && <Loader2 className="size-3.5 animate-spin" />}{t("common.save")}</Button></DialogFooter>
      </form>}
    </DialogContent>
  </Dialog>;
}
