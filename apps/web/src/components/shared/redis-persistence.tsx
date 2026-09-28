"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { HardDriveDownload, Loader2 } from "lucide-react";
import type { RedisPersistence, RedisSnapshotReceipt } from "@nsb/schema";
import * as api from "@/lib/api";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";

export function redisSnapshotState(receipt: RedisSnapshotReceipt, current: RedisPersistence) {
  if (receipt.version !== current.version || receipt.runId !== current.runId) return "restarted";
  if (current.loading || current.saving) return "waiting";
  if (current.lastSaveStatus === "err") return "failed";
  return current.lastSaveTime >= receipt.minimumSaveTime ? "complete" : "waiting";
}

export function RedisPersistenceButton({ version, running }: { version?: string | null; running?: boolean }) {
  const t = useT();
  const [target, setTarget] = React.useState<string | null>(null);
  return <><Button variant="secondary" size="sm" disabled={!version || !running} onClick={() => setTarget(version!)}><HardDriveDownload className="size-3.5" />{t("redisPersistence.title")}</Button>
    {target && <RedisPersistenceDialog key={target} version={target} onClose={() => setTarget(null)} />}</>;
}

function RedisPersistenceDialog({ version, onClose }: { version: string; onClose: () => void }) {
  const t = useT();
  const samples = React.useRef(0);
  const query = useQuery({ queryKey: ["redis-persistence", version], queryFn: async () => {
    const sample = ++samples.current;
    return { sample, report: await api.redisPersistence(version) };
  }, retry: false, gcTime: 0, refetchInterval: 2000 });
  const [receipt, setReceipt] = React.useState<RedisSnapshotReceipt | null>(null);
  const [afterSample, setAfterSample] = React.useState(0);
  const [requestedAt, setRequestedAt] = React.useState(0);
  const [result, setResult] = React.useState<"complete" | "failed" | "restarted" | null>(null);
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const mounted = React.useRef(true);
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const errorRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => { if (error || query.isError) errorRef.current?.focus(); }, [error, query.isError]);
  const current = query.isError ? undefined : query.data?.report;
  // 只使用接受请求之后发起的读取；上次失败或在途旧读取不能判定新快照的结果。
  const outcome = result ?? (receipt && current && query.data!.sample > afterSample ? redisSnapshotState(receipt, current) : null);
  React.useEffect(() => { if (!result && outcome && outcome !== "waiting") setResult(outcome); }, [result, outcome]);
  const waiting = !!receipt && (!outcome || outcome === "waiting");
  const slow = waiting && Date.now() - requestedAt > 120000;
  const unavailable = !current || current.loading || current.saving || current.aofRewriting || current.aofRewriteScheduled;
  const failure = error ?? (query.isError ? normalizeError(query.error) : null);
  const request = async () => {
    if (busyRef.current || waiting || unavailable) return;
    busyRef.current = true; setBusy(true); setError(null); setReceipt(null); setResult(null);
    try {
      const accepted = await api.redisSnapshot(version);
      if (mounted.current) { setAfterSample(samples.current); setReceipt(accepted); setRequestedAt(Date.now()); void query.refetch(); }
    } catch (e) { if (mounted.current) setError(normalizeError(e)); }
    finally { busyRef.current = false; if (mounted.current) setBusy(false); }
  };
  const status = (value: string | null) => t(value === "ok" ? "redisPersistence.ok" : value === "err" ? "redisPersistence.error" : "redisPersistence.unknown");
  const row = (label: string, value: React.ReactNode) => <div className="grid grid-cols-1 gap-1 sm:grid-cols-2 sm:gap-4"><dt className="text-muted">{label}</dt><dd className="min-w-0 font-medium [overflow-wrap:anywhere]">{value}</dd></div>;
  return <Dialog open onOpenChange={(open) => { if (!open && !busyRef.current) onClose(); }}>
    <DialogContent hideClose={busy} className="flex max-h-[90dvh] max-w-xl flex-col overflow-hidden p-4 sm:p-6">
      <DialogHeader className="shrink-0 pr-6"><DialogTitle className="leading-snug">Redis {version} · {t("redisPersistence.title")}</DialogTitle><DialogDescription>{t("redisPersistence.intro")}</DialogDescription></DialogHeader>
      <div className="min-h-0 space-y-4 overflow-y-auto px-0.5 text-xs leading-relaxed">
        {!isTauri && <p className="text-warn">{t("redisPersistence.demo")}</p>}
        {query.isPending && <p role="status">{t("common.loading")}</p>}
        {failure && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-1 rounded-lg bg-error-soft p-3 text-error outline-none [overflow-wrap:anywhere]"><p>{failure.message}</p>{failure.hint && <p>{failure.hint}</p>}</div>}
        {receipt && <p role="status" className={`rounded-lg p-3 ${outcome === "failed" || outcome === "restarted" || slow ? "bg-warn-soft text-warn" : "bg-fill text-secondary"}`}>{t(slow ? "redisPersistence.slow" : `redisPersistence.${outcome ?? "waiting"}`)}</p>}
        {current && <>
          <section className="space-y-3"><h3 className="font-semibold">{t("redisPersistence.rdb")}</h3><dl className="space-y-3">
            {row(t("redisPersistence.state"), t(current.loading ? "redisPersistence.loading" : current.saving ? "redisPersistence.saving" : "redisPersistence.idle"))}
            {row(t("redisPersistence.changes"), current.changesSinceSave.toLocaleString())}
            {row(t("redisPersistence.lastStatus"), status(current.lastSaveStatus))}
            {row(t("redisPersistence.lastTime"), new Date(current.lastSaveTime * 1000).toLocaleString())}
            {row(t("redisPersistence.duration"), current.lastSaveDuration === null ? t("redisPersistence.unknown") : `${current.lastSaveDuration} ${t("redisPersistence.seconds")}`)}
          </dl><p className="text-faint">{t("redisPersistence.timeHint")}</p></section>
          <section className="space-y-3 border-t border-dashed border-separator pt-4"><h3 className="font-semibold">{t("redisPersistence.aof")}</h3><dl className="space-y-3">
            {row(t("redisPersistence.enabled"), t(current.aofEnabled ? "redisPersistence.on" : "redisPersistence.off"))}
            {row(t("redisPersistence.rewrite"), t(current.aofRewriting ? "redisPersistence.rewriting" : current.aofRewriteScheduled ? "redisPersistence.scheduled" : "redisPersistence.idle"))}
            {current.aofEnabled && <>{row(t("redisPersistence.writeStatus"), status(current.aofLastWriteStatus))}{row(t("redisPersistence.rewriteStatus"), status(current.aofLastRewriteStatus))}</>}
          </dl></section>
        </>}
        <div className="space-y-2 border-t border-dashed border-separator pt-3 text-muted"><p>{t("redisPersistence.scope")}</p><p>{t("redisPersistence.closeHint")}</p>{unavailable && current && <p className="text-warn">{t("redisPersistence.busyHint")}</p>}</div>
      </div>
      <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-3">
        <Button variant="ghost" size="sm" disabled={busy || query.isFetching} onClick={() => { setError(null); void query.refetch(); }}>{t("redisPersistence.refresh")}</Button>
        <Button variant="ghost" size="sm" disabled={busy} onClick={onClose}>{t("common.close")}</Button>
        <Button size="sm" disabled={busy || waiting || !!unavailable} onClick={() => void request()}>{(busy || waiting) && <Loader2 className="size-3.5 animate-spin motion-reduce:animate-none" />}{t(busy ? "redisPersistence.requesting" : waiting ? "redisPersistence.tracking" : "redisPersistence.create")}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
