"use client";

import * as React from "react";
import Link from "next/link";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2, RefreshCw } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { isTauri, normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { CopyButton } from "@/components/shared/misc";

export function SiteNetworkDialog({ id, name, onClose }: { id: string; name: string; onClose: () => void }) {
  const t = useT();
  const client = useQueryClient();
  const [busy, setBusy] = React.useState(false);
  const guard = React.useRef(false);
  const [draft, setDraft] = React.useState<boolean | null>(null);
  const [ip, setIp] = React.useState("");
  const [error, setError] = React.useState<string | null>(null);
  const query = useQuery({ queryKey: ["site-network", id], queryFn: () => api.siteNetworkInfo(id),
    enabled: !busy, retry: false, networkMode: "always", refetchInterval: 5000 });
  const data = query.data;
  const ready = !!data && !query.error;
  const enabled = draft ?? data?.enabled ?? false;
  const selected = data?.addresses.find((entry) => `${entry.interface}|${entry.address}` === ip) ?? data?.addresses[0];
  const listening = ready && !!selected?.listening;
  const url = ready ? data.accessUrl : null;
  const host = url ? new URL(url).hostname : "";
  const dnsLine = selected && host ? `${selected.address} ${host}` : "";
  const toggleId = React.useId();
  const addressId = React.useId();
  React.useEffect(() => { setDraft(null); setError(null); }, [data?.server]);
  const apply = async () => {
    if (guard.current || !ready) return;
    guard.current = true; setBusy(true); setError(null);
    try {
      await api.siteNetworkApply(id, data.server, enabled);
      setDraft(null);
      toast.success(t(data.running ? "lan.applied" : "lan.saved"));
    } catch (error) {
      const detail = normalizeError(error);
      setError([detail.message, detail.hint, detail.detail].filter(Boolean).join("\n"));
    } finally {
      await Promise.all([query.refetch(), ...["sites", "services", "tunnel-sites", "tunnels"].map((key) => client.invalidateQueries({ queryKey: [key] }))]);
      guard.current = false; setBusy(false);
    }
  };

  return <Dialog open onOpenChange={(open) => { if (!open && !guard.current) onClose(); }}>
    <DialogContent className="flex max-h-[85dvh] max-w-2xl flex-col overflow-hidden p-4 sm:p-6" hideClose={busy}>
      <DialogHeader className="shrink-0 pr-8">
        <DialogTitle className="break-words leading-snug">{t("lan.title")} · {name}</DialogTitle>
        <DialogDescription className="text-xs leading-relaxed">{t("lan.description")}</DialogDescription>
      </DialogHeader>
      <div className="min-h-0 space-y-4 overflow-y-auto pr-1 text-xs">
        {!isTauri && <p className="rounded-lg bg-info-soft p-3 text-info">{t("lan.preview")}</p>}
        {query.isPending && <p role="status" className="py-4 text-muted">{t("common.loading")}</p>}
        {query.error && <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-error">
          <span className="min-w-0 flex-1 break-words">{normalizeError(query.error).message}</span>
          <Button size="sm" variant="secondary" disabled={query.isFetching || busy} onClick={() => void query.refetch()}>{t("install.retry")}</Button>
        </div>}
        {data && <>
          <section className="space-y-2 rounded-xl border border-border p-3">
            <div className="flex items-center justify-between gap-4">
              <label htmlFor={toggleId} className="min-w-0 font-medium">{t("lan.enable")} · {data.server === "nginx" ? "Nginx" : data.server === "apache" ? "Apache" : "Caddy"}</label>
              <Switch id={toggleId} checked={enabled} disabled={busy || !ready} onCheckedChange={(next) => { setDraft(next); setError(null); }} />
            </div>
            <p className="leading-relaxed text-muted">{t("lan.scope")}</p>
            <details className="text-muted"><summary className="cursor-pointer">{t("lan.affected")} · {data.affectedSites.length}</summary>
              <ul className="mt-2 list-inside list-disc space-y-1 break-words">{data.affectedSites.map((name, index) => <li key={index}>{name}</li>)}</ul>
            </details>
            <p className="text-muted">{t(data.running ? "lan.restartHint" : "lan.stoppedHint")}</p>
          </section>
          <section className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <label id={addressId} className="font-medium">{t("lan.interface")}</label>
              <Button size="sm" variant="ghost" disabled={query.isFetching || busy} onClick={() => void query.refetch()}><RefreshCw className="h-3 w-3" />{t("lan.refresh")}</Button>
            </div>
            {data.addresses.length ? <Select value={selected ? `${selected.interface}|${selected.address}` : ""} onValueChange={setIp} disabled={busy || !ready}>
              <SelectTrigger aria-labelledby={addressId} className="w-full min-w-0 text-xs"><SelectValue /></SelectTrigger>
              <SelectContent>{data.addresses.map((entry) => <SelectItem key={`${entry.interface}|${entry.address}`} value={`${entry.interface}|${entry.address}`} className="text-xs">
                {entry.interface} · {entry.address}
              </SelectItem>)}</SelectContent>
            </Select> : <p className="rounded-lg bg-fill p-3 text-muted">{t("lan.noAddress")}</p>}
            <p role="status" className={listening ? "text-running" : "text-muted"}>{t(listening ? "lan.listening" : "lan.notListening")}</p>
          </section>
          <section className="space-y-3 border-t border-dashed border-separator pt-3">
            <p className="font-medium">{t("lan.clientSetup")}</p>
            <p className="leading-relaxed text-muted">{t("lan.dnsHint")}</p>
            {dnsLine && <div className="flex items-center gap-2 rounded-lg bg-fill p-3"><code className="min-w-0 flex-1 break-all">{dnsLine}</code><CopyButton text={dnsLine} /></div>}
            {url && listening ? <div className="flex items-center gap-2 rounded-lg bg-fill p-3"><code className="min-w-0 flex-1 break-all">{url}</code><CopyButton text={url} /></div>
              : <p className="text-muted">{t("lan.addressPending")}</p>}
            {data.localCa && <p className="leading-relaxed text-muted">{t("lan.caHint")}{" "}<Link href="/tls" className="underline underline-offset-2">{t("lan.certificates")}</Link></p>}
            <p className="leading-relaxed text-muted">{t("lan.firewallHint")}</p>
          </section>
        </>}
        {error && <p role="alert" className="whitespace-pre-wrap break-words text-error">{error}</p>}
      </div>
      <DialogFooter className="shrink-0 flex-wrap border-t border-dashed border-separator pt-3">
        <Button variant="secondary" disabled={busy} onClick={onClose}>{t("common.cancel")}</Button>
        <Button disabled={!ready || busy} onClick={() => void apply()}>{busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{t(busy ? "lan.applying" : data?.running ? "lan.applyRestart" : "lan.save")}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
