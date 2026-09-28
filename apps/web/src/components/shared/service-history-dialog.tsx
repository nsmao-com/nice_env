"use client";

import * as React from "react";
import { History, Loader2, RefreshCw } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import type { ServiceStatus } from "@nsb/schema";
import { useT } from "@/lib/store";
import { normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";

export function ServiceHistoryDialog({ services }: { services: ServiceStatus[] }) {
  const t = useT();
  const [open, setOpen] = React.useState(false);
  const [filter, setFilter] = React.useState("all");
  const [query, setQuery] = React.useState("");
  const history = useQuery({
    queryKey: ["service-history"],
    queryFn: () => api.serviceHistory(200),
    enabled: open,
    staleTime: 0,
    retry: false,
  });
  const labels = React.useMemo(() => new Map(services.map((service) => [service.id, service.label])), [services]);
  const rows = (history.data ?? []).filter((entry) => {
    if (filter !== "all" && entry.serviceId !== filter) return false;
    const text = `${labels.get(entry.serviceId) ?? entry.serviceId} ${entry.detail}`.toLowerCase();
    return !query.trim() || text.includes(query.trim().toLowerCase());
  });
  const error = history.error ? normalizeError(history.error) : null;

  return (
    <>
      <Button variant="ghost" size="sm" onClick={() => setOpen(true)}>
        <History className="h-3.5 w-3.5" />
        {t("logs.history")}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="flex max-h-[calc(100dvh-2rem)] max-w-2xl flex-col gap-0 overflow-hidden p-0">
          <DialogHeader className="shrink-0 px-4 py-4 pr-12 sm:px-5">
            <DialogTitle className="text-[14px]">{t("logs.historyTitle")}</DialogTitle>
            <DialogDescription className="text-[11.5px] leading-relaxed">{t("logs.historyHint")}</DialogDescription>
          </DialogHeader>
          <div className="mx-4 shrink-0 border-t border-dashed border-separator sm:mx-5" />
          <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-4 py-4 sm:px-5">
            <div className="flex flex-col gap-2 sm:flex-row">
              <Select value={filter} onValueChange={setFilter}>
                <SelectTrigger className="h-9 min-w-0 sm:w-56" aria-label={t("logs.historyFilter")}>
                  <SelectValue placeholder={t("logs.historyAll")} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">{t("logs.historyAll")}</SelectItem>
                  {services.map((service) => <SelectItem key={service.id} value={service.id}>{service.label}</SelectItem>)}
                </SelectContent>
              </Select>
              <Input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t("logs.historySearch")} aria-label={t("logs.historySearch")} className="h-9 text-xs" />
            </div>
            {history.isFetching && <p role="status" className="flex items-center gap-2 text-xs text-muted"><Loader2 className="h-3.5 w-3.5 animate-spin" />{t("logs.historyLoading")}</p>}
            {error && <div role="alert" className="space-y-2 rounded-lg bg-error-soft px-3 py-2 text-xs text-error"><p>{error.message}</p><Button size="sm" variant="secondary" disabled={history.isFetching} onClick={() => void history.refetch()}>{t("log.refresh")}</Button></div>}
            {!history.isFetching && !error && rows.length === 0 && <p role="status" className="py-8 text-center text-xs text-muted">{t("logs.historyEmpty")}</p>}
            {rows.length > 0 && <ol className="space-y-1.5">
              {rows.map((entry, index) => <li key={`${entry.ts}-${entry.serviceId}-${index}`} className="flex min-w-0 flex-col gap-1 rounded-lg border border-border/60 bg-card-2/20 px-3 py-2.5 sm:flex-row sm:items-start sm:gap-3">
                <time className="shrink-0 font-mono text-[10.5px] text-faint" dateTime={new Date(entry.ts).toISOString()}>{new Date(entry.ts).toLocaleString()}</time>
                <span className="min-w-0 flex-1 break-words text-xs"><strong className="font-medium">{labels.get(entry.serviceId) ?? entry.serviceId}</strong><span className="mx-1.5 text-faint">·</span>{entry.detail}</span>
              </li>)}
            </ol>}
          </div>
          <DialogFooter className="shrink-0 border-t border-dashed border-separator px-4 py-3 sm:px-5">
            <Button variant="ghost" size="sm" disabled={history.isFetching} onClick={() => void history.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("logs.historyRefresh")}</Button>
            <Button size="sm" onClick={() => setOpen(false)}>{t("common.close")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
