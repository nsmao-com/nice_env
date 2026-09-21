"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  Activity,
  AlertCircle,
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  Info,
  Loader2,
  RefreshCw,
} from "lucide-react";
import type { HealthReport } from "@nsb/schema";
import { useT } from "@/lib/store";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/misc";

/**
 * 环境体检。
 *
 * R1–R9 的检查项散落在各页面，用户不会逐个去点。这里聚合成一张清单并按
 * 严重程度排序，每条都带「去处理」——直接跳到能修它的那个页面。
 */
export function HealthCard() {
  const t = useT();
  const router = useRouter();
  const [report, setReport] = React.useState<HealthReport | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [dismissed, setDismissed] = React.useState<Set<string>>(new Set());

  const load = React.useCallback(async (silent = false) => {
    if (!silent) setLoading(true);
    try {
      setReport(await api.healthCheck());
    } catch {
      /* 后端未就绪时不阻塞界面 */
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => {
    void load();
  }, [load]);

  const visible = (report?.items ?? []).filter((i) => !dismissed.has(i.id));

  return (
    <Card>
      <CardHeader className="flex-row items-center gap-3">
        <div
          className={cn(
            "flex h-9 w-9 items-center justify-center rounded-lg border",
            report && report.errors > 0
              ? "border-error/30 bg-error-soft"
              : report && report.warnings > 0
                ? "border-warn/30 bg-warn-soft"
                : "border-running/30 bg-running-soft"
          )}
        >
          <Activity
            className={cn(
              "h-4 w-4",
              report && report.errors > 0
                ? "text-error"
                : report && report.warnings > 0
                  ? "text-warn"
                  : "text-running"
            )}
            strokeWidth={1.8}
          />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{t("health.title")}</CardTitle>
          <p className="mt-0.5 text-[11px] text-muted">
            {loading && !report ? t("common.loading") : (report?.summary ?? "—")}
          </p>
        </div>
        <Button size="sm" variant="ghost" className="h-8" onClick={() => void load()} disabled={loading}>
          <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
        </Button>
      </CardHeader>
      <CardContent>
        {loading && !report ? (
          <div className="space-y-1.5">
            {Array.from({ length: 3 }).map((_, i) => (
              <Skeleton key={i} className="h-10 w-full" />
            ))}
          </div>
        ) : visible.length === 0 ? (
          <div className="flex items-center justify-center gap-2 py-5 text-[12.5px] text-running">
            <CheckCircle2 className="h-4 w-4" />
            {t("health.allOk")}
          </div>
        ) : (
          <div className="space-y-1.5">
            {visible.map((item) => {
              const Icon =
                item.severity === "error" ? AlertCircle : item.severity === "warn" ? AlertTriangle : Info;
              const tone =
                item.severity === "error"
                  ? "text-error"
                  : item.severity === "warn"
                    ? "text-warn"
                    : "text-info";
              const border =
                item.severity === "error"
                  ? "border-error/25 bg-error-soft/40"
                  : item.severity === "warn"
                    ? "border-warn/25 bg-warn-soft/40"
                    : "border-border/60 bg-card-2/25";
              return (
                <div key={item.id} className={cn("rounded-lg border px-2.5 py-2", border)}>
                  <div className="flex items-start gap-2.5">
                    <Icon className={cn("mt-0.5 h-3.5 w-3.5 shrink-0", tone)} strokeWidth={2} />
                    <div className="min-w-0 flex-1">
                      <p className="text-[12.5px] font-medium">{item.title}</p>
                      {item.detail && (
                        <p className="mt-0.5 break-words text-[11px] leading-relaxed text-muted">
                          {item.detail}
                        </p>
                      )}
                      {item.action && (
                        <p className="mt-0.5 text-[11px] text-faint">{item.action}</p>
                      )}
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      {item.route && (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-6 px-1.5 text-[11px]"
                          onClick={() => router.push(item.route!)}
                        >
                          {t("health.fix")}
                          <ChevronRight className="ml-0.5 h-3 w-3" />
                        </Button>
                      )}
                      {item.severity !== "error" && (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-6 px-1.5 text-[11px] text-faint"
                          onClick={() => setDismissed((s) => new Set(s).add(item.id))}
                        >
                          {t("health.ignore")}
                        </Button>
                      )}
                    </div>
                  </div>
                </div>
              );
            })}
            {dismissed.size > 0 && (
              <button
                type="button"
                className="w-full pt-1 text-center text-[10.5px] text-faint hover:text-muted"
                onClick={() => setDismissed(new Set())}
              >
                {t("health.showIgnored").replace("{n}", String(dismissed.size))}
              </button>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
