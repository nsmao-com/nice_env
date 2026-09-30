"use client";

import * as React from "react";
import { ChevronLeft, ChevronRight, Download, Play, X } from "lucide-react";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CodeEditor } from "@/components/shared/code-editor";
import { ConfirmDialog } from "@/components/shared/misc";
import type { PostgresDatabaseInfo } from "@/lib/api";

const RESULT_PAGE_SIZE = 100;

export function PostgresWorkspace({ version, databases, ready, targetLabel, onLockChange }: {
  version: string;
  databases: PostgresDatabaseInfo[];
  ready: boolean;
  targetLabel: string;
  onLockChange: (locked: boolean) => void;
}) {
  const t = useT();
  const available = databases.filter(database => database.allowConnections);
  const [database, setDatabase] = React.useState("");
  const [sql, setSql] = React.useState("SELECT current_database() AS database, current_user AS user;");
  const [confirmSql, setConfirmSql] = React.useState<string | null>(null);
  const [result, setResult] = React.useState<api.PostgresQueryResult | null>(null);
  const [error, setError] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const selected = available.some(item => item.name === database) ? database : "";
  const sqlValid = sql.trim().length > 0 && new TextEncoder().encode(sql).length <= 1024 * 1024 && !sql.includes("\0");
  React.useEffect(() => {
    if (!selected && available[0]) setDatabase(available[0].name);
  }, [available, selected]);
  React.useEffect(() => {
    onLockChange(busy || confirmSql !== null);
    return () => onLockChange(false);
  }, [busy, confirmSql, onLockChange]);
  const run = async () => {
    if (!confirmSql || busy || !selected || !sqlValid) return;
    setBusy(true); setError(""); setResult(null);
    try {
      setResult(await api.postgresQuery(version, { database: selected, sql: confirmSql, confirmed: true }));
      setConfirmSql(null);
    } catch (cause) {
      const parsed = normalizeError(cause);
      setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" "));
      setConfirmSql(null);
    } finally { setBusy(false); }
  };
  const exportCsv = () => {
    if (!result) return;
    const escape = (value: string) => `"${value.replaceAll("\"", "\"\"")}"`;
    const content = [result.columns, ...result.rows].map(row => row.map(escape).join(",")).join("\r\n");
    const url = URL.createObjectURL(new Blob(["\ufeff", content], { type: "text/csv;charset=utf-8" }));
    const link = document.createElement("a"); link.href = url; link.download = "postgres-query-result.csv"; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return <Card className="min-w-0">
    <CardHeader><div className="flex flex-wrap items-start justify-between gap-2"><div className="min-w-0"><CardTitle>{t("pgWorkspace.title")}</CardTitle><p className="mt-1 text-xs leading-5 text-muted">{t("pgWorkspace.intro")}</p></div><span className="text-xs text-muted">{targetLabel}</span></div></CardHeader>
    <CardContent className="space-y-4">
      <div className="flex min-w-0 flex-wrap items-end gap-2"><div className="min-w-0 flex-1 basis-56 space-y-1.5"><label htmlFor="pg-workspace-database" className="text-xs font-medium">{t("pgWorkspace.database")}</label><Select value={selected} disabled={!ready || busy || !!confirmSql} onValueChange={value => { setDatabase(value); setResult(null); setError(""); }}><SelectTrigger id="pg-workspace-database"><SelectValue placeholder={t("pgWorkspace.chooseDatabase")} /></SelectTrigger><SelectContent>{available.map(item => <SelectItem key={item.oid} value={item.name} className="whitespace-normal break-all">{item.name}</SelectItem>)}</SelectContent></Select></div><Button type="button" size="sm" variant="ghost" disabled={!ready || busy} onClick={() => { setResult(null); setError(""); }}><X className="h-3.5 w-3.5" />{t("pgWorkspace.clear")}</Button></div>
      {!ready ? <p className="rounded-lg bg-fill p-3 text-sm text-muted">{t("pgWorkspace.notReady")}</p> : !available.length ? <p className="rounded-lg bg-fill p-3 text-sm text-muted">{t("pgWorkspace.noDatabase")}</p> : <>
        <CodeEditor label={t("pgWorkspace.sql")} language="sql" value={sql} onChange={setSql} readOnly={busy || !!confirmSql} height="230px" />
        {!sqlValid && <p role="alert" className="text-xs text-error">{t("pgWorkspace.sqlInvalid")}</p>}
        <div className="flex flex-wrap items-center gap-3"><Button type="button" disabled={!selected || !sqlValid || busy} onClick={() => { setError(""); setConfirmSql(sql); }}><Play className="h-3.5 w-3.5" />{busy ? t("pgWorkspace.running") : t("pgWorkspace.run")}</Button><span className="text-xs leading-5 text-muted">{t("pgWorkspace.limitHint")}</span></div>
        {error && <p role="alert" className="break-words rounded-lg bg-error/5 p-3 text-xs leading-5 text-error">{error}</p>}
        {result && <PostgresResultGrid result={result} onExport={exportCsv} />}
      </>}
      <ConfirmDialog open={confirmSql !== null} onOpenChange={open => { if (!open && !busy) setConfirmSql(null); }} title={t("pgWorkspace.confirmTitle")} description={t("pgWorkspace.confirmDesc").replace("{database}", selected || "—").replace("{target}", targetLabel)} confirmText={t("pgWorkspace.confirm")} loading={busy} confirmDisabled={!ready || !selected} onConfirm={() => void run()}><pre className="max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-fill p-3 font-mono text-xs leading-5">{confirmSql}</pre></ConfirmDialog>
    </CardContent>
  </Card>;
}

function PostgresResultGrid({ result, onExport }: { result: api.PostgresQueryResult; onExport: () => void }) {
  const t = useT();
  const [page, setPage] = React.useState(0);
  React.useEffect(() => setPage(0), [result]);
  const pageCount = Math.max(1, Math.ceil(result.rows.length / RESULT_PAGE_SIZE));
  const visibleRows = result.rows.slice(page * RESULT_PAGE_SIZE, (page + 1) * RESULT_PAGE_SIZE);
  return <div className="min-w-0 space-y-2 border-t border-dashed border-border pt-4"><div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted"><span>{result.columns.length ? `${result.rows.length} ${t("pgWorkspace.rows")}` : t("pgWorkspace.noRows")}</span>{result.columns.length > 0 && <Button type="button" size="sm" variant="ghost" onClick={onExport}><Download className="h-3.5 w-3.5" />{t("pgWorkspace.export")}</Button>}</div>{!result.columns.length ? <p className="rounded-lg bg-fill p-4 text-sm text-running">{t("pgWorkspace.executed")}</p> : <div className="max-h-96 min-w-0 overflow-auto rounded-lg border border-border"><table className="w-full min-w-max border-collapse text-left text-xs"><thead className="sticky top-0 z-10 bg-card"><tr>{result.columns.map((column, index) => <th key={index} className="whitespace-nowrap border-b border-border px-3 py-2 font-medium">{column}</th>)}</tr></thead><tbody>{result.rows.map((row, rowIndex) => <tr key={rowIndex} className="border-b border-border/50 align-top hover:bg-fill">{result.columns.map((_, columnIndex) => <td key={columnIndex} className="max-w-80 whitespace-pre-wrap break-words px-3 py-2 font-mono">{row[columnIndex] ?? ""}</td>)}</tr>)}</tbody></table></div>}</div>;
  return <div className="min-w-0 space-y-2 border-t border-dashed border-border pt-4"><div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted"><span>{result.columns.length ? `${result.rows.length} ${t("pgWorkspace.rows")}` : t("pgWorkspace.noRows")}</span>{result.columns.length > 0 && <Button type="button" size="sm" variant="ghost" onClick={onExport}><Download className="h-3.5 w-3.5" />{t("pgWorkspace.export")}</Button>}</div>{!result.columns.length ? <p className="rounded-lg bg-fill p-4 text-sm text-running">{t("pgWorkspace.executed")}</p> : <><div className="max-h-96 min-w-0 overflow-auto rounded-lg border border-border"><table className="w-full min-w-max border-collapse text-left text-xs"><thead className="sticky top-0 z-10 bg-card"><tr>{result.columns.map((column, index) => <th key={index} className="whitespace-nowrap border-b border-border px-3 py-2 font-medium">{column}</th>)}</tr></thead><tbody>{visibleRows.map((row, rowIndex) => <tr key={page * RESULT_PAGE_SIZE + rowIndex} className="border-b border-border/50 align-top hover:bg-fill">{result.columns.map((_, columnIndex) => <td key={columnIndex} className="max-w-80 whitespace-pre-wrap break-words px-3 py-2 font-mono">{row[columnIndex] ?? ""}</td>)}</tr>)}</tbody></table></div>{result.rows.length > RESULT_PAGE_SIZE && <div className="flex flex-wrap items-center gap-2"><Button type="button" size="sm" variant="secondary" disabled={!page} onClick={() => setPage(current => current - 1)}><ChevronLeft className="h-3.5 w-3.5" />{t("pgWorkspace.previous")}</Button><span className="text-xs text-muted">{t("pgWorkspace.page").replace("{page}", String(page + 1)).replace("{pages}", String(pageCount))}</span><Button type="button" size="sm" variant="secondary" disabled={page + 1 >= pageCount} onClick={() => setPage(current => current + 1)}>{t("pgWorkspace.next")}<ChevronRight className="h-3.5 w-3.5" /></Button></div>}</>}</div>;
}
