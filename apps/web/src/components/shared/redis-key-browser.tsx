"use client";

import * as React from "react";
import { ChevronLeft, ChevronRight, Eye, Loader2, RefreshCw, Search, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CopyButton } from "@/components/shared/misc";

const PAGE_SIZE = 40;
type Translator = ReturnType<typeof useT>;

function formatTtl(t: Translator, ttlMs: number) {
  if (ttlMs === -1) return t("redisBrowser.noExpiry");
  if (ttlMs < 0) return t("redisBrowser.expired");
  if (ttlMs < 1000) return `${ttlMs} ms`;
  if (ttlMs < 60_000) return `${Math.ceil(ttlMs / 1000)} s`;
  if (ttlMs < 3_600_000) return `${Math.ceil(ttlMs / 60_000)} min`;
  return `${Math.ceil(ttlMs / 3_600_000)} h`;
}

function formatBytes(bytes: number | undefined) {
  if (bytes == null) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function QueryError({ error, busy, retry }: { error: unknown; busy: boolean; retry: () => void }) {
  const t = useT();
  const parsed = normalizeError(error);
  return <div className="space-y-2"><p role="alert" className="break-words text-sm text-error">{parsed.message}</p>{parsed.hint && <p className="break-words text-xs leading-5 text-muted">{parsed.hint}</p>}<Button type="button" size="sm" variant="secondary" disabled={busy} onClick={retry}>{t("redisBrowser.retry")}</Button></div>;
}

export function RedisKeyBrowser({ version, running, signature }: { version?: string; running: boolean; signature: string }) {
  const t = useT();
  const [database, setDatabase] = React.useState("0");
  const [patternDraft, setPatternDraft] = React.useState("");
  const [pattern, setPattern] = React.useState("");
  const [cursor, setCursor] = React.useState("0");
  const [history, setHistory] = React.useState<string[]>([]);
  const [selectedKey, setSelectedKey] = React.useState<string | null>(null);
  const [flushOpen, setFlushOpen] = React.useState(false);
  const [flushConfirmation, setFlushConfirmation] = React.useState("");
  const [flushBusy, setFlushBusy] = React.useState(false);
  const [flushError, setFlushError] = React.useState("");
  const [deleteKey, setDeleteKey] = React.useState<string | null>(null);
  const [deleteConfirmation, setDeleteConfirmation] = React.useState("");
  const [deleteBusy, setDeleteBusy] = React.useState(false);
  const [deleteError, setDeleteError] = React.useState("");
  const queryClient = useQueryClient();
  const patternValid = new TextEncoder().encode(patternDraft).length <= 256 && !/[\x00-\x1f\x7f]/.test(patternDraft);
  const query = useQuery({
    queryKey: ["redis-keys", signature, database, cursor, pattern],
    queryFn: () => api.redisKeys({ version: version!, database: Number(database), cursor, pattern, count: PAGE_SIZE }),
    enabled: running && !!version,
    retry: false,
    refetchOnWindowFocus: false,
  });
  const reset = (nextDatabase = database, nextPattern = pattern) => {
    setDatabase(nextDatabase); setPattern(nextPattern); setCursor("0"); setHistory([]);
  };
  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!patternValid || query.isFetching) return;
    const next = patternDraft;
    if (next === pattern && cursor === "0") void query.refetch();
    else reset(database, next);
  };
  const nextCursor = query.data?.nextCursor;
  const canNext = !!nextCursor && nextCursor !== "0";
  const goNext = () => {
    if (!canNext || query.isFetching) return;
    setHistory(previous => [...previous, cursor]);
    setCursor(nextCursor!);
  };
  const goPrevious = () => {
    if (!history.length || query.isFetching) return;
    const previous = history[history.length - 1];
    setHistory(history.slice(0, -1)); setCursor(previous);
  };
  const submitFlush = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!version || !running || flushBusy || flushConfirmation.trim() !== "FLUSHDB") return;
    setFlushBusy(true); setFlushError("");
    try {
      await api.redisFlush(version, Number(database), flushConfirmation.trim());
      setFlushOpen(false); setFlushConfirmation(""); setCursor("0"); setHistory([]);
      await query.refetch();
      toast.success(t("redisBrowser.flushed").replace("{n}", database));
    } catch (error) {
      setFlushError(normalizeError(error).message);
    } finally { setFlushBusy(false); }
  };
  const submitDelete = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!version || !running || !deleteKey || deleteBusy || deleteConfirmation !== deleteKey) return;
    setDeleteBusy(true); setDeleteError("");
    try {
      await api.redisKeyDelete(version, Number(database), deleteKey, deleteConfirmation);
      setDeleteKey(null); setDeleteConfirmation(""); setCursor("0"); setHistory([]);
      await queryClient.invalidateQueries({ queryKey: ["redis-keys", signature, database] });
      toast.success(t("redisBrowser.deleted"));
    } catch (error) {
      setDeleteError(normalizeError(error).message);
    } finally { setDeleteBusy(false); }
  };
  return <>
    <Card className="min-w-0">
      <CardHeader><div className="flex flex-wrap items-start justify-between gap-3"><div className="min-w-0"><CardTitle>{t("redisBrowser.title")}</CardTitle><p className="mt-1 text-xs leading-5 text-muted">{t("redisBrowser.intro")}</p></div><div className="flex shrink-0 flex-wrap items-center gap-2"><Badge variant="outline">{t("redisBrowser.keyActions")}</Badge><Button type="button" size="sm" variant="destructive" disabled={!running || !version || query.isFetching || flushBusy || deleteBusy} onClick={() => { setFlushError(""); setFlushConfirmation(""); setFlushOpen(true); }}><Trash2 className="h-3.5 w-3.5" />{t("redisBrowser.flush")}</Button></div></div></CardHeader>
      <CardContent className="space-y-4">
        <form className="grid min-w-0 gap-3 sm:grid-cols-[9rem_minmax(0,1fr)_auto] sm:items-end" onSubmit={submit}>
          <div className="min-w-0 space-y-1.5"><Label htmlFor="redis-browser-db">{t("redisBrowser.database")}</Label><Select value={database} disabled={query.isFetching} onValueChange={value => reset(value, pattern)}><SelectTrigger id="redis-browser-db"><SelectValue /></SelectTrigger><SelectContent>{Array.from({ length: 16 }, (_, index) => <SelectItem key={index} value={String(index)}>{t("redisBrowser.databaseNumber").replace("{n}", String(index))}</SelectItem>)}</SelectContent></Select></div>
          <div className="min-w-0 space-y-1.5"><Label htmlFor="redis-browser-pattern">{t("redisBrowser.pattern")}</Label><Input id="redis-browser-pattern" value={patternDraft} maxLength={256} placeholder="*" aria-invalid={!patternValid} disabled={query.isFetching} onChange={event => setPatternDraft(event.target.value)} /></div>
          <Button type="submit" variant="secondary" disabled={!patternValid || query.isFetching}><Search className="h-3.5 w-3.5" />{t("redisBrowser.apply")}</Button>
        </form>
        {!patternValid && <p role="alert" className="text-xs text-error">{t("redisBrowser.patternInvalid")}</p>}
        <p className="text-xs leading-5 text-muted">{t("redisBrowser.patternHint")}</p>
        <div className="border-t border-dashed border-border" />
        {query.isPending && <div role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 animate-spin" />{t("redisBrowser.loading")}</div>}
        {query.isError && <QueryError error={query.error} busy={query.isFetching} retry={() => void query.refetch()} />}
        {!query.isFetching && !query.isError && query.data && <>
          <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted"><span>{t("redisBrowser.pageCount").replace("{n}", String(query.data.items.length))}</span><Button type="button" size="sm" variant="ghost" disabled={query.isFetching} onClick={() => void query.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("redisBrowser.refresh")}</Button></div>
          {!query.data.items.length ? <div className="rounded-lg bg-fill px-3 py-6 text-center"><p className="text-sm text-muted">{t("redisBrowser.empty")}</p><p className="mt-1 text-xs leading-5 text-muted">{t("redisBrowser.emptyHint")}</p></div> : <div className="min-w-0 space-y-2">
             <div className="min-w-0 space-y-2">{query.data.items.map(item => <div key={item.key} className="flex min-w-0 flex-wrap items-center gap-2 rounded-lg bg-fill px-3 py-2.5"><button type="button" className="min-w-0 flex-1 truncate text-left font-mono text-xs text-primary underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary" title={item.key} onClick={() => setSelectedKey(item.key)}>{item.key}</button><Badge variant="muted">{item.keyType}</Badge><span className="shrink-0 text-[11px] text-muted">{formatTtl(t, item.ttlMs)}</span><Button type="button" size="icon" variant="ghost" className="h-8 w-8 shrink-0" aria-label={t("redisBrowser.inspect")} title={t("redisBrowser.inspect")} onClick={() => setSelectedKey(item.key)}><Eye className="h-3.5 w-3.5" /></Button><Button type="button" size="icon" variant="ghost" className="h-8 w-8 shrink-0 text-error hover:text-error" aria-label={t("redisBrowser.delete")} title={t("redisBrowser.delete")} disabled={deleteBusy || flushBusy} onClick={() => { setDeleteError(""); setDeleteConfirmation(""); setDeleteKey(item.key); }}><Trash2 className="h-3.5 w-3.5" /></Button></div>)}</div>
          </div>}
          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-dashed border-border pt-3"><span className="text-xs text-muted">{t("redisBrowser.scanHint")}</span><div className="flex gap-2"><Button type="button" size="sm" variant="secondary" disabled={!history.length || query.isFetching} onClick={goPrevious}><ChevronLeft className="h-3.5 w-3.5" />{t("redisBrowser.previous")}</Button><Button type="button" size="sm" variant="secondary" disabled={!canNext || query.isFetching} onClick={goNext}>{t("redisBrowser.next")}<ChevronRight className="h-3.5 w-3.5" /></Button></div></div>
        </>}
      </CardContent>
    </Card>
    <Dialog open={flushOpen} onOpenChange={(open) => { if (!flushBusy) { setFlushOpen(open); if (!open) { setFlushConfirmation(""); setFlushError(""); } } }}>
      <DialogContent className="max-w-md" hideClose={flushBusy}>
        <DialogHeader>
          <DialogTitle>{t("redisBrowser.flushTitle").replace("{n}", database)}</DialogTitle>
          <DialogDescription>{t("redisBrowser.flushHint")}</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={submitFlush}>
          <div className="space-y-1.5">
            <Label htmlFor="redis-flush-confirm">{t("redisBrowser.flushConfirm")}</Label>
            <Input id="redis-flush-confirm" value={flushConfirmation} autoComplete="off" spellCheck={false} disabled={flushBusy} placeholder={t("redisBrowser.flushPlaceholder")} onChange={(event) => setFlushConfirmation(event.target.value)} />
          </div>
          {flushError && <p role="alert" className="break-words text-sm text-error">{flushError}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row">
            <Button type="button" variant="ghost" disabled={flushBusy} onClick={() => setFlushOpen(false)}>{t("redisBrowser.flushCancel")}</Button>
            <Button type="submit" variant="destructive" disabled={flushBusy || flushConfirmation.trim() !== "FLUSHDB"}>{flushBusy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Trash2 className="h-3.5 w-3.5" />}{flushBusy ? t("redisBrowser.flushing") : t("redisBrowser.flushAction")}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={deleteKey !== null} onOpenChange={(open) => { if (!deleteBusy && !open) { setDeleteKey(null); setDeleteConfirmation(""); setDeleteError(""); } }}>
      <DialogContent className="max-w-md" hideClose={deleteBusy}>
        <DialogHeader><DialogTitle>{t("redisBrowser.deleteTitle")}</DialogTitle><DialogDescription>{t("redisBrowser.deleteHint")}</DialogDescription></DialogHeader>
        <form className="space-y-4" onSubmit={submitDelete}>
          <div className="rounded-lg bg-warn-soft p-3"><p className="text-xs text-warn">{t("redisBrowser.deleteWarning")}</p><p className="mt-2 break-all font-mono text-xs text-foreground">{deleteKey}</p></div>
          <div className="space-y-1.5"><Label htmlFor="redis-key-delete-confirm">{t("redisBrowser.deleteConfirm")}</Label><Input id="redis-key-delete-confirm" value={deleteConfirmation} autoComplete="off" spellCheck={false} disabled={deleteBusy} placeholder={t("redisBrowser.deletePlaceholder")} onChange={event => setDeleteConfirmation(event.target.value)} /></div>
          {deleteError && <p role="alert" className="break-words text-sm text-error">{deleteError}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={deleteBusy} onClick={() => setDeleteKey(null)}>{t("redisBrowser.deleteCancel")}</Button><Button type="submit" variant="destructive" disabled={deleteBusy || !deleteKey || deleteConfirmation !== deleteKey}>{deleteBusy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Trash2 className="h-3.5 w-3.5" />}{deleteBusy ? t("redisBrowser.deleting") : t("redisBrowser.deleteAction")}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    {selectedKey && version && <RedisKeyPreviewDialog key={`${signature}:${database}:${selectedKey}`} version={version} database={Number(database)} keyName={selectedKey} onClose={() => setSelectedKey(null)} />}
  </>;
}

function RedisKeyPreviewDialog({ version, database, keyName, onClose }: { version: string; database: number; keyName: string; onClose: () => void }) {
  const t = useT();
  const query = useQuery({ queryKey: ["redis-key-preview", version, database, keyName], queryFn: () => api.redisKeyPreview({ version, database, key: keyName }), retry: false, refetchOnWindowFocus: false });
  const parsedError = query.error ? normalizeError(query.error) : null;
  return <Dialog open onOpenChange={open => !open && onClose()}><DialogContent className="flex max-h-[85dvh] max-w-2xl flex-col overflow-hidden"><DialogHeader><DialogTitle className="pr-6">{t("redisBrowser.previewTitle")}</DialogTitle><DialogDescription className="break-all font-mono">{keyName}</DialogDescription></DialogHeader><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
    {query.isPending && <p role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 animate-spin" />{t("redisBrowser.previewLoading")}</p>}
    {parsedError && <div className="space-y-2"><p role="alert" className="break-words text-sm text-error">{parsedError.message}</p>{parsedError.hint && <p className="break-words text-xs leading-5 text-muted">{parsedError.hint}</p>}<Button type="button" size="sm" variant="secondary" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("redisBrowser.retry")}</Button></div>}
    {query.data && <>
      <div className="grid min-w-0 gap-3 sm:grid-cols-3"><div className="min-w-0 rounded-lg bg-fill p-3"><p className="text-[11px] text-muted">{t("redisBrowser.type")}</p><p className="mt-1 break-all text-sm font-medium">{query.data.keyType}</p></div><div className="min-w-0 rounded-lg bg-fill p-3"><p className="text-[11px] text-muted">{t("redisBrowser.ttl")}</p><p className="mt-1 text-sm font-medium">{formatTtl(t, query.data.ttlMs)}</p></div><div className="min-w-0 rounded-lg bg-fill p-3"><p className="text-[11px] text-muted">{t("redisBrowser.memory")}</p><p className="mt-1 text-sm font-medium">{formatBytes(query.data.memoryBytes)}</p></div></div>
      {query.data.elements != null && <p className="text-xs text-muted">{t("redisBrowser.elements").replace("{n}", String(query.data.elements))}</p>}
      {query.data.value != null ? <div className="min-w-0 space-y-2"><div className="flex flex-wrap items-center justify-between gap-2"><p className="text-sm font-medium">{t("redisBrowser.value")}</p>{!query.data.valueTruncated && <CopyButton text={query.data.value} />}</div><pre tabIndex={0} className="max-h-72 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-fill p-3 font-mono text-xs leading-5">{query.data.value}</pre>{query.data.valueTruncated && <p className="text-xs leading-5 text-warn">{t("redisBrowser.valueTruncated")}</p>}</div> : <p className="rounded-lg bg-fill p-3 text-xs leading-5 text-muted">{t("redisBrowser.collectionHint")}</p>}
    </>}
  </div><DialogFooter><Button type="button" variant="ghost" onClick={onClose}>{t("common.close")}</Button></DialogFooter></DialogContent></Dialog>;
}
