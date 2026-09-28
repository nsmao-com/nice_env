"use client";

import * as React from "react";
import type { ProjectPlatformReport } from "@nsb/schema";
import { Loader2, Puzzle, RefreshCw } from "lucide-react";
import { useT } from "@/lib/store";
import { usePackages } from "@/lib/hooks";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { PhpExtensionsDialog } from "@/components/shared/php-extensions";

type Props = { project?: string; siteId?: string; savedRoot?: string; version: string; disabled?: boolean; directoryChanged?: boolean };

/** 项目、版本或安装元数据变化后丢弃旧报告，异步结果只属于发起时的面板。 */
export function ProjectPlatformCheck(props: Props) {
  const { data: packages } = usePackages();
  const installed = packages.filter((p) => (p.id === "php" && p.version === props.version) || p.id === "composer");
  const available = installed.some((p) => p.id === "php" && p.install);
  const identity = JSON.stringify([props.project, props.siteId, props.savedRoot, props.version, props.directoryChanged, installed.map((p) => [p.id, p.version, p.install])]);
  return <PlatformPanel key={identity} {...props} available={available} />;
}

function PlatformPanel({ project, siteId, version, disabled = false, directoryChanged = false, available }: Props & { available: boolean }) {
  const t = useT();
  const [includeDev, setIncludeDev] = React.useState(false);
  const [report, setReport] = React.useState<ProjectPlatformReport | null>(null);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [checking, setChecking] = React.useState(false);
  const [extensionsOpen, setExtensionsOpen] = React.useState(false);
  const [stale, setStale] = React.useState(false);
  const [showPassed, setShowPassed] = React.useState(false);
  const [page, setPage] = React.useState(0);
  const mounted = React.useRef(false);
  const busy = React.useRef(false);
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const blocked = disabled || checking || directoryChanged || !available || !version || !isTauri;
  const check = async () => {
    if (blocked || busy.current) return;
    busy.current = true; setChecking(true); setReport(null); setError(null); setStale(false); setPage(0); setShowPassed(false);
    try {
      const next = await api.projectPlatformCheck(siteId ? { siteId } : { project }, version, includeDev);
      if (mounted.current) setReport(next);
    } catch (e) {
      if (mounted.current) setError(normalizeError(e));
    } finally {
      busy.current = false;
      if (mounted.current) setChecking(false);
    }
  };
  const issues = report?.requirements.filter((r) => r.status !== "success") ?? [];
  const rows = report?.requirements.filter((r) => showPassed || r.status !== "success") ?? [];
  const pages = Math.max(1, Math.ceil(rows.length / 6));
  const status = checking ? "platformCheck.checking" : !report ? "platformCheck.idle" : !report.requirements.length ? "platformCheck.empty"
    : issues.length ? "platformCheck.problems" : "platformCheck.passed";
  const manageExtensions = (open: boolean) => {
    setExtensionsOpen(open); setReport(null); setError(null); setStale(true);
  };
  return <section className="min-w-0 space-y-3 rounded-xl border border-border p-4" aria-busy={checking}>
    <div className="space-y-1">
      <h3 className="text-sm font-medium">{t("platformCheck.title")}{version && <span className="ml-2 inline-block font-mono text-xs text-muted">PHP {version}</span>}</h3>
      <p className="text-xs leading-relaxed text-muted">{t("platformCheck.intro")}</p>
    </div>
    <div className="flex flex-wrap items-center justify-between gap-3">
      <label className="flex items-center gap-2 text-xs text-secondary">
        <input type="checkbox" className="size-4 accent-primary" checked={includeDev} disabled={disabled || checking}
          onChange={(event) => { setIncludeDev(event.target.checked); setReport(null); setError(null); setStale(true); }} />
        {t("platformCheck.dev")}
      </label>
      <div className="flex flex-wrap gap-2">
        <Button type="button" size="sm" variant="ghost" disabled={blocked} onClick={() => manageExtensions(true)}>
          <Puzzle className="size-3.5" />{t("platformCheck.extensions")}
        </Button>
        <Button type="button" size="sm" variant="secondary" disabled={blocked} onClick={() => void check()}>
          {checking ? <Loader2 className="size-3.5 animate-spin motion-reduce:animate-none" /> : <RefreshCw className="size-3.5" />}
          {t(checking ? "platformCheck.checking" : "platformCheck.run")}
        </Button>
      </div>
    </div>
    {!isTauri ? <p className="text-xs text-muted">{t("platformCheck.desktop")}</p>
      : directoryChanged ? <p className="text-xs text-warn">{t("platformCheck.saveDirectory")}</p>
      : !available ? <p className="text-xs text-muted">{t("platformCheck.selectPhp")}</p>
      : <p role="status" className={`text-xs leading-relaxed ${issues.length ? "text-warn" : "text-secondary"}`}>{t(stale ? "platformCheck.stale" : status).replace("{count}", String(issues.length))}</p>}
    {error && <div role="alert" className="space-y-1 rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">
      <p className="[overflow-wrap:anywhere]">{error.message}</p>
      {error.hint && <p>{error.hint}</p>}
      {error.detail && <details className="pt-1"><summary className="cursor-pointer">{t("platformCheck.details")}</summary><pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap [overflow-wrap:anywhere]">{error.detail}</pre></details>}
    </div>}
    {report && <div className="min-w-0 space-y-3 border-t border-dashed border-separator pt-3">
      <p className="text-xs text-muted">{t(`platformCheck.source.${report.source}`)} · PHP {report.phpVersion}</p>
      {report.source === "manifest" && <p className="text-xs leading-relaxed text-warn">{t("platformCheck.manifestOnly")}</p>}
      {report.source === "lock" && report.lockFresh !== true && <p className="text-xs leading-relaxed text-warn">{t(report.lockFresh === false ? "platformCheck.lockStale" : "platformCheck.lockUnknown")}</p>}
      {report.devIncomplete && <p className="text-xs leading-relaxed text-warn">{t("platformCheck.devIncomplete")}</p>}
      {report.diagnostics && <p className="text-xs leading-relaxed text-warn">{t("platformCheck.warnings")}</p>}
      {!report.autoloadPresent && <p className="text-xs leading-relaxed text-warn">{t("platformCheck.noAutoload")}</p>}
      {!!report.requirements.length && <label className="flex items-center gap-2 text-xs text-secondary">
        <input type="checkbox" className="size-4 accent-primary" checked={showPassed} onChange={(event) => { setShowPassed(event.target.checked); setPage(0); }} />
        {t("platformCheck.showPassed").replace("{count}", String(report.requirements.length - issues.length))}
      </label>}
      {rows.length > 0 && <ul className="space-y-2">
        {rows.slice(page * 6, (page + 1) * 6).map((row, index) => <li key={`${row.name}-${index}`} className="min-w-0 space-y-1 rounded-lg bg-fill p-3 text-xs leading-relaxed">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <span className="font-mono font-medium [overflow-wrap:anywhere]">{row.name}</span>
            <span className={row.status === "success" ? "text-muted" : "text-warn"}>{t(`platformCheck.${row.status}`)}</span>
          </div>
          {row.status !== "missing" && <p className="text-muted [overflow-wrap:anywhere]">{t("platformCheck.actual").replace("{version}", row.version)}</p>}
          {row.failedRequirement && <p className="text-secondary [overflow-wrap:anywhere]">{row.failedRequirement.source} · {row.failedRequirement.constraint}</p>}
          {row.provider && <p className="text-muted [overflow-wrap:anywhere]">{row.provider}</p>}
        </li>)}
      </ul>}
      {pages > 1 && <div className="flex flex-wrap items-center justify-end gap-2 text-xs text-muted">
        <Button type="button" size="sm" variant="ghost" disabled={page === 0} onClick={() => setPage((n) => n - 1)}>{t("platformCheck.previous")}</Button>
        <span aria-live="polite">{page + 1} / {pages}</span>
        <Button type="button" size="sm" variant="ghost" disabled={page + 1 === pages} onClick={() => setPage((n) => n + 1)}>{t("platformCheck.next")}</Button>
      </div>}
      <details className="text-xs text-muted">
        <summary className="cursor-pointer">{t("platformCheck.details")}</summary>
        <div className="mt-2 space-y-2 [overflow-wrap:anywhere]"><p>{report.project}</p><p>{report.ini}</p>
          {report.diagnostics && <pre className="max-h-48 overflow-auto whitespace-pre-wrap">{report.diagnostics}</pre>}
        </div>
      </details>
    </div>}
    <p className="text-xs leading-relaxed text-faint">{t("platformCheck.scope")}</p>
    <PhpExtensionsDialog version={version || null} open={extensionsOpen} onOpenChange={manageExtensions} />
  </section>;
}
