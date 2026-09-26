"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { Activity, AlertCircle, AlertTriangle, CheckCircle2, ChevronRight, Info, RefreshCw } from "lucide-react";
import type { HealthReport } from "@nsb/schema";
import { useT } from "@/lib/store";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/misc";

/** 保留上次快照；同一查询共享正在进行的请求，隐藏只影响当前报告的显示。 */
export function HealthCard() {
  const t = useT();
  const router = useRouter();
  const health = useQuery({
    queryKey: ["health-report"], queryFn: api.healthCheck, retry: false,
    staleTime: 60_000, refetchOnWindowFocus: false, refetchOnReconnect: false, networkMode: "always",
  });
  const report = health.data;
  const loading = health.isFetching;
  const error = health.error ? normalizeError(health.error) : null;
  const [dismissed, setDismissed] = React.useState<{ report?: HealthReport; ids: Set<string> }>({ ids: new Set() });
  const visible = (report?.items ?? []).filter((item) => dismissed.report !== report || !dismissed.ids.has(item.id));
  const hidden = (report?.items.length ?? 0) - visible.length;
  const incomplete = !!report && (report.checks.length === 0 || report.checks.some((check) => check.state === "unavailable"));
  const tone = !report || error ? "text-muted" : report.errors > 0 ? "text-error" : report.warnings > 0 || incomplete ? "text-warn" : report.infos > 0 ? "text-info" : "text-running";
  const load = () => {
    setDismissed({ ids: new Set() });
    // 不取消已有 native 调用再启动一次，跨页面返回也复用同一个请求。
    void health.refetch({ cancelRefetch: false });
  };

  return <Card className="min-w-0" aria-label={t("health.title")}>
    <CardHeader className="flex-row flex-wrap items-center gap-3">
      <div className={cn("flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-card-2", tone)}>
        <Activity className="h-4 w-4" strokeWidth={1.8} />
      </div>
      <div className="min-w-0 flex-1 [overflow-wrap:anywhere]">
        <CardTitle className="text-[13px]">{t("health.title")}</CardTitle>
        <p className="mt-1 text-[11px] text-muted" role="status">
          {report?.summary ?? t(loading ? "common.loading" : error ? "health.readFailed" : "health.pending")}
        </p>
      </div>
      <Button size="sm" variant="ghost" className="h-9 w-9 shrink-0 p-0" onClick={load} disabled={loading} aria-label={t("health.retry")} title={t("health.retry")}>
        <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
      </Button>
    </CardHeader>
    <CardContent className="min-w-0 space-y-3 [overflow-wrap:anywhere]">
      {error && <div role="alert" className="rounded-lg border border-error/25 bg-error-soft/40 px-3 py-2.5 text-xs text-error">
        <p>{t(report ? "health.refreshFailed" : "health.readFailed")}</p>
        <p className="mt-1 text-[11px]">{error.message}</p>
        {error.hint && <p className="mt-1 text-[11px]">{error.hint}</p>}
        {!report && <Button size="sm" variant="secondary" className="mt-2" disabled={loading} onClick={load}>{t("health.retry")}</Button>}
      </div>}
      {loading && <div role="status" className="text-[11px] text-muted">
        {t(report ? "health.refreshing" : "health.checking")}
        {!report && <div className="mt-2 space-y-1.5" aria-hidden="true">{[0, 1, 2].map((id) => <Skeleton key={id} className="h-10 w-full" />)}</div>}
      </div>}
      {report && <>
        <p className="text-[11px] leading-relaxed text-muted">
          {t("health.checkedAt")} <time dateTime={new Date(report.checkedAt * 1000).toISOString()}>{new Date(report.checkedAt * 1000).toLocaleString()}</time>
          <span className="mt-0.5 block">{t("health.snapshotHint")}</span>
        </p>
        {visible.length === 0 ? <div className={cn("flex items-start gap-2 rounded-lg bg-card-2/40 px-3 py-3 text-xs", hidden > 0 || incomplete || error ? "text-muted" : "text-running")}>
          {hidden > 0 || incomplete || error ? <Info className="mt-0.5 h-4 w-4 shrink-0" /> : <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" />}
          <p>{t(hidden > 0 ? "health.allHidden" : incomplete ? "health.incomplete" : error ? "health.stale" : "health.allOk")}</p>
        </div> : <div className="space-y-2">
          {visible.map((item) => {
            const Icon = item.severity === "error" ? AlertCircle : item.severity === "warn" ? AlertTriangle : Info;
            const color = item.severity === "error" ? "text-error" : item.severity === "warn" ? "text-warn" : "text-info";
            return <div key={item.id} className={cn("min-w-0 rounded-lg border px-3 py-2.5", item.severity === "error" ? "border-error/25 bg-error-soft/40" : item.severity === "warn" ? "border-warn/25 bg-warn-soft/40" : "border-border/60 bg-card-2/25")}>
              <div className="flex items-start gap-2">
                <Icon className={cn("mt-0.5 h-3.5 w-3.5 shrink-0", color)} />
                <div className="min-w-0 flex-1">
                  <p className="text-[12.5px] font-medium">{item.title}</p>
                  {item.detail && <p className="mt-1 text-[11px] leading-relaxed text-muted">{item.detail}</p>}
                  {item.action && <p className="mt-1 text-[11px] leading-relaxed text-faint">{item.action}</p>}
                </div>
              </div>
              <div className="mt-2 flex flex-wrap justify-end gap-1">
                {item.route && <Button size="sm" variant="ghost" className="min-h-8 h-auto whitespace-normal px-2 text-[11px]" onClick={() => router.push(item.route!)}>
                  {t("health.fix")}<ChevronRight className="h-3 w-3 shrink-0" />
                </Button>}
                {item.severity !== "error" && <Button size="sm" variant="ghost" className="min-h-8 h-auto whitespace-normal px-2 text-[11px] text-muted"
                  onClick={() => setDismissed((current) => ({ report, ids: new Set(current.report === report ? current.ids : []).add(item.id) }))}>
                  {t("health.ignore")}
                </Button>}
              </div>
            </div>;
          })}
        </div>}
        {hidden > 0 && <div className="space-y-1">
          <p className="text-[11px] text-muted">{t("health.hiddenHint")}</p>
          <Button size="sm" variant="ghost" className="min-h-9 h-auto max-w-full whitespace-normal text-[11px]" onClick={() => setDismissed({ ids: new Set() })}>
            {t("health.showIgnored").replace("{n}", String(hidden))}
          </Button>
        </div>}
        <details className="mx-1 border-t border-dashed border-border pt-3 text-[11px]">
          <summary className="cursor-pointer rounded py-1 text-muted focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring">
            {t("health.coverage")}{incomplete && <span className="ml-2 text-warn">{t("health.incompleteShort")}</span>}
          </summary>
          <div className="mt-2 space-y-3">
            {report.checks.length === 0 && <p className="text-muted">{t("health.coverageMissing")}</p>}
            {report.checks.map((check) => <div key={check.id}>
              <div className="flex flex-wrap items-start gap-x-2 gap-y-1">
                <span className="font-medium">{check.label}</span>
                <span className={check.state === "unavailable" ? "text-warn" : "text-muted"}>{t(`health.scope.${check.state}`)}</span>
              </div>
              <p className="mt-0.5 leading-relaxed text-muted">{check.detail}</p>
            </div>)}
            <p className="text-faint">{t("health.coverageHint")}</p>
          </div>
        </details>
      </>}
    </CardContent>
  </Card>;
}
