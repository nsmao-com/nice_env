"use client";

import type { ProjectPhpCompatibility } from "@nsb/schema";
import { useT } from "@/lib/store";
import { cmpVersionDesc } from "@/lib/utils";
import { Button } from "@/components/ui/button";

export function projectPhpProblem(report: ProjectPhpCompatibility | null | undefined, version: string, acknowledged: boolean) {
  if (report?.status === "invalid") return "projectPhp.invalid" as const;
  if (report?.status === "unspecified") return null;
  if (report?.status === "checked" && report.versions.includes(version)) {
    return report.matchingVersions.includes(version) ? null : "projectPhp.incompatible" as const;
  }
  return acknowledged ? null : "projectPhp.confirmRequired" as const;
}

export function recommendedProjectPhp(report: ProjectPhpCompatibility | null | undefined, installed: string[], preferred = "") {
  const eligible = report?.status === "checked" ? installed.filter((v) => report.matchingVersions.includes(v))
    : report?.status === "unspecified" ? installed : [];
  return eligible.includes(preferred) ? preferred : [...eligible].sort(cmpVersionDesc)[0] ?? "";
}

/** 扫描列表与完整向导共用；无法校验的确认不能绕过已知版本冲突。 */
export function ProjectPhpCheck({ report, version, acknowledged, onAcknowledge, onRefresh, loading = false, disabled = false }: {
  report?: ProjectPhpCompatibility | null; version: string; acknowledged: boolean;
  onAcknowledge: (value: boolean) => void; onRefresh: () => void; loading?: boolean; disabled?: boolean;
}) {
  const t = useT();
  const unknown = !report || report.status === "unavailable" || (report.status === "checked" && !!version && !report.versions.includes(version));
  const incompatible = report?.status === "checked" && !!version && report.versions.includes(version) && !report.matchingVersions.includes(version);
  const message = loading ? "projectPhp.checking" : report?.status === "invalid" ? "projectPhp.invalid"
    : unknown ? "projectPhp.unavailable" : report?.status === "unspecified" ? "projectPhp.unspecified"
    : !report?.matchingVersions.length ? "projectPhp.none" : incompatible ? "projectPhp.incompatible" : !version ? "projectPhp.choose" : "projectPhp.checked";
  return <div className="min-w-0 space-y-2 rounded-lg bg-fill p-3 text-xs leading-relaxed">
    <div className="flex flex-wrap items-start justify-between gap-2">
      <p role="status" className={incompatible || report?.status === "invalid" ? "text-error" : "text-muted"}>{t(message)}</p>
      <Button type="button" size="sm" variant="ghost" disabled={disabled || loading} onClick={onRefresh}>{t("projectPhp.refresh")}</Button>
    </div>
    {report?.requirement && <p className="font-mono text-secondary [overflow-wrap:anywhere]">PHP {report.requirement}</p>}
    {!loading && report?.message && <p className="text-muted [overflow-wrap:anywhere]">{report.message}</p>}
    {!loading && report?.status === "checked" && report.matchingVersions.length > 0 && <p className="text-muted [overflow-wrap:anywhere]">{t("projectPhp.matches").replace("{versions}", report.matchingVersions.join("、"))}</p>}
    {!loading && unknown && <label className="flex items-start gap-2 text-secondary">
      <input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={acknowledged} disabled={disabled || !version} onChange={(event) => onAcknowledge(event.target.checked)} />
      <span>{t("projectPhp.confirm")}</span>
    </label>}
    <p className="text-faint">{t("projectPhp.scope")}</p>
  </div>;
}
