"use client";

import * as React from "react";
import Link from "next/link";
import { toast } from "sonner";
import { Globe2, Loader2, ExternalLink, Square, Trash2 } from "lucide-react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "@/lib/api";
import type { TunnelInfo } from "@/lib/api";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem, SelectSeparator } from "@/components/ui/select";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { CopyButton } from "@/components/shared/misc";

export function TunnelPanel({ siteId }: { siteId?: string }) {
  const t = useT();
  const client = useQueryClient();
  const query = useQuery({ queryKey: ["tunnels"], queryFn: api.tunnelList, refetchInterval: 2500, retry: false, networkMode: "always" });
  const sites = useQuery({ queryKey: ["tunnel-sites"], queryFn: api.listSites, refetchInterval: 5000, retry: false, networkMode: "always" });
  const [chosenTarget, setTarget] = React.useState("");
  const target = siteId ? `site:${siteId}` : chosenTarget;
  const [port, setPort] = React.useState("8080");
  const [starting, setStarting] = React.useState(false);
  const startGuard = React.useRef(false);
  const [startError, setStartError] = React.useState<string | null>(null);
  const [working, setWorking] = React.useState<string[]>([]);
  const actions = React.useRef(new Set<string>());
  const [expanded, setExpanded] = React.useState<string | null>(null);
  const targetId = React.useId();
  const selectedSite = sites.data?.find((site) => `site:${site.id}` === target);
  const validTarget = target === "custom" || (!!selectedSite && selectedSite.status === "running" && !!selectedSite.accessUrl && !sites.error);
  const rows = (query.data ?? []).filter((row) => !siteId || row.siteId === siteId)
    .sort((a, b) => Number(b.alive) - Number(a.alive) || b.startedAt - a.startedAt);
  const active = (query.data ?? []).some((row) => row.alive && (siteId || selectedSite
    ? row.siteId === (siteId ?? selectedSite?.id)
    : target === "custom" && !row.siteId && row.port === Number(port)));
  const refresh = () => query.refetch();
  const start = async (retry?: TunnelInfo) => {
    if (startGuard.current) return;
    const siteId = retry ? retry.siteId : selectedSite?.id;
    const localPort = retry?.port ?? Number(port);
    if ((!retry && !validTarget) || (!retry && active)) return;
    if (!siteId && (!Number.isInteger(localPort) || localPort < 1 || localPort > 65535)) {
      setStartError(t("tools.tunnel.badPort")); return;
    }
    startGuard.current = true; setStarting(true); setStartError(null);
    try {
      const info = siteId ? await api.tunnelStartSite(siteId) : await api.tunnelStart(localPort);
      await client.cancelQueries({ queryKey: ["tunnels"] });
      client.setQueryData<TunnelInfo[]>(["tunnels"], (old) => [...(old ?? []).filter((row) => row.id !== info.id), info]);
      if (info.state === "failed") { setExpanded(info.id); toast.error(info.error || t("tools.tunnel.failed")); }
      else toast.message(t(info.state === "connected" ? "tools.tunnel.connected" : "tools.tunnel.starting"));
    } catch (e) { const error = normalizeError(e); setStartError([error.message, error.hint].filter(Boolean).join("\n")); if (retry) toastError(e); }
    finally { await refresh(); startGuard.current = false; setStarting(false); }
  };
  const act = async (id: string, action: () => Promise<unknown>) => {
    if (actions.current.has(id)) return;
    actions.current.add(id); setWorking([...actions.current]);
    try { await action(); } catch (e) { toastError(e); }
    finally { await refresh(); actions.current.delete(id); setWorking([...actions.current]); }
  };

  return (
      <div className="flex min-w-0 flex-col gap-3">
        {!isTauri && <p className="rounded-lg bg-info-soft p-3 text-xs text-info">{t("tools.tunnel.preview")}</p>}
        <form className="flex min-w-0 flex-col gap-2" onSubmit={(event) => { event.preventDefault(); void start(); }}>
          {!siteId && <>
          <label id={targetId} className="text-xs text-muted">{t("tools.tunnel.target")}</label>
          <Select value={target} onValueChange={(value) => { setTarget(value); setStartError(null); }} disabled={starting}>
            <SelectTrigger className="min-w-0 text-xs" aria-labelledby={targetId}><SelectValue placeholder={t("tools.tunnel.choose")} /></SelectTrigger>
            <SelectContent>
              {(sites.data ?? []).map((site) => <SelectItem key={site.id} value={`site:${site.id}`} disabled={site.status !== "running" || !site.accessUrl || !!sites.error} className="text-xs [&>span:last-child]:min-w-0 [&>span:last-child]:break-all">
                {site.name} · {site.accessUrl || site.domains[0]}{site.status !== "running" ? ` · ${t("tools.tunnel.siteStopped")}` : !site.accessUrl ? ` · ${t("tools.tunnel.addressPending")}` : ""}
              </SelectItem>)}
              {!!sites.data?.length && <SelectSeparator />}
              <SelectItem value="custom" className="text-xs">{t("tools.tunnel.custom")}</SelectItem>
            </SelectContent>
          </Select>
          </>}
          {sites.isPending && <p role="status" className="text-[11px] text-faint">{t("tools.tunnel.loadingSites")}</p>}
          {sites.error && <div role="alert" className="flex flex-wrap items-center gap-2 text-xs text-error">
            <span className="min-w-0 flex-1 break-words">{t(siteId ? "sites.share.loadFailed" : "tools.tunnel.sitesFailed")} · {normalizeError(sites.error).message}</span>
            <Button type="button" size="sm" variant="secondary" disabled={sites.isFetching} onClick={() => void sites.refetch()}>{t("install.retry")}</Button>
          </div>}
          {target === "custom" && <label className="space-y-1.5 text-xs text-muted">{t("tools.tunnel.port")}
            <Input type="number" min={1} max={65535} step={1} required value={port} disabled={starting} onChange={(event) => setPort(event.target.value)} className="font-mono text-xs" />
          </label>}
          {siteId && !selectedSite && !sites.isPending && !sites.error && <p role="status" className="text-xs text-warn">{t("sites.share.missing")}</p>}
          {selectedSite && <p role="status" className="break-all rounded-lg bg-card-2/50 p-3 text-xs text-secondary">
            {validTarget ? <>{t("tools.tunnel.forwardTo")} <span className="font-mono">{selectedSite.accessUrl}</span></> : t("tools.tunnel.addressUnavailable")}
          </p>}
          <p className="text-[11px] leading-relaxed text-muted">{t(siteId ? "sites.share.hint" : "tools.tunnel.targetHint")}</p>
          {startError && <p role="alert" className="whitespace-pre-line break-words text-xs text-error">{startError}</p>}
          <Button type="submit" size="sm" className="self-start" disabled={starting || active || query.isPending || !!query.error || !validTarget}>
            {starting ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Globe2 className="h-3.5 w-3.5" />}{t(siteId ? "sites.share.start" : "tools.tunnel.start")}
          </Button>
          {active && <p role="status" className="text-[11px] text-muted">{t("sites.share.active")}</p>}
          <p className="text-[11px] text-muted">{t("sites.share.requires")}{" "}
            <Link className="underline underline-offset-2" href="/packages?search=cloudflared">{t("sites.share.install")}</Link>
          </p>
        </form>
        {query.error && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
          <span className="min-w-0 flex-1 break-words">{t("tools.tunnel.readFailed")} · {normalizeError(query.error).message}</span>
          <Button size="sm" variant="secondary" disabled={query.isFetching} onClick={() => void refresh()}>{t("install.retry")}</Button>
        </div>}
        {query.isPending ? <p role="status" className="py-4 text-center text-xs text-faint">{t("common.loading")}</p>
          : !query.error && !rows.length ? <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-faint">{t(siteId ? "sites.share.none" : "tools.tunnel.none")}</p> : null}
        {rows.map((tn) => <section key={tn.id} aria-label={tn.target} className="min-w-0 rounded-lg border border-border/60 p-3">
          <p className="break-all font-mono text-xs">{tn.target}</p>
          <div className="mt-2 flex flex-wrap items-center gap-2 text-[11px]">
            <Badge role="status" variant={tn.state === "connected" ? "running" : tn.state === "failed" ? "error" : "info"}>{t(`tools.tunnel.${tn.state}`)}</Badge>
            <time className="text-faint" title={new Date(tn.startedAt).toLocaleString()}>{new Date(tn.startedAt).toLocaleString()}</time>
          </div>
          {tn.alive && tn.localReachable === false && <p role="status" className="mt-2 text-xs text-warn">{t("tools.tunnel.localDown")}</p>}
          {tn.error && <p role="alert" className="mt-2 break-words text-xs text-error">{tn.error}</p>}
          {tn.url ? <p className="mt-2 break-all font-mono text-[11px] text-secondary">{tn.url}</p>
            : tn.alive && <p className="mt-2 text-[11px] text-faint">{t("tools.tunnel.pending")}</p>}
          <div className="mt-2 flex flex-wrap items-center gap-1 border-t border-dashed border-border pt-2">
            {tn.url && <>
              {!query.error && tn.alive && tn.state === "connected" && tn.localReachable && <CopyButton text={tn.url} />}
              <Button size="sm" variant="ghost" disabled={!isTauri || !!query.error || !tn.alive || tn.state !== "connected" || !tn.localReachable}
                onClick={() => void api.openInBrowser(tn.url!).catch(toastError)}><ExternalLink className="h-3.5 w-3.5" />{t("common.open")}</Button>
            </>}
            <Button size="sm" variant="ghost" aria-expanded={expanded === tn.id} onClick={() => setExpanded(expanded === tn.id ? null : tn.id)}>{t("tools.tunnel.output")}</Button>
            {tn.alive ? <Button size="sm" variant="secondary" disabled={working.includes(tn.id)} onClick={() => void act(tn.id, () => api.tunnelStop(tn.id))}>
              {working.includes(tn.id) ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Square className="h-3.5 w-3.5" />}{t(siteId ? "sites.share.stop" : "tools.tunnel.stop")}
            </Button> : <>
              <Button size="sm" variant="secondary" disabled={starting || working.includes(tn.id) || !!query.error || (!!siteId && (!validTarget || active))} onClick={() => void start(tn)}>{t("tools.tunnel.restart")}</Button>
              <Button size="icon-sm" variant="ghost" className="ml-auto text-faint hover:text-error" aria-label={`${t("tools.tunnel.remove")} · ${tn.target}`} disabled={working.includes(tn.id) || !!query.error} onClick={() => void act(tn.id, () => api.tunnelRemove(tn.id))}><Trash2 className="h-3.5 w-3.5" /></Button>
            </>}
          </div>
          {expanded === tn.id && <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-card-2/50 p-3 font-mono text-[11px] leading-relaxed text-secondary">{tn.logs.join("\n") || t("tools.tunnel.noOutput")}</pre>}
        </section>)}
        {siteId ? <details className="text-[11px] leading-relaxed text-muted">
          <summary className="cursor-pointer rounded focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary">{t("sites.share.notes")}</summary>
          <p className="mt-2">{t("tools.tunnel.hint2")}</p>
        </details> : <p className="text-[11px] leading-relaxed text-muted">{t("tools.tunnel.hint2")}</p>}
      </div>

  );
}

/** 工具箱和站点入口共享同一隧道列表与生命周期；关闭面板不会停止分享。 */
export function SiteShareDialog({ siteId, name, onClose }: { siteId: string; name: string; onClose: () => void }) {
  const t = useT();
  return <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
    <DialogContent className="max-h-[85dvh] max-w-xl overflow-y-auto p-4 sm:p-6">
      <DialogHeader className="min-w-0 pr-8">
        <DialogTitle className="break-words leading-snug">{t("sites.share.title")} · {name}</DialogTitle>
        <DialogDescription className="text-xs leading-relaxed">{t("sites.share.description")}</DialogDescription>
      </DialogHeader>
      <TunnelPanel key={siteId} siteId={siteId} />
    </DialogContent>
  </Dialog>;
}
