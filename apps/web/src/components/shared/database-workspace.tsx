"use client";
import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import type { DatabaseEngine } from "@nsb/schema";
import * as api from "@/lib/api";
import { toastError } from "@/lib/hooks";
import { normalizeError } from "@/lib/backend";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Switch } from "@/components/ui/switch";
import { CodeEditor } from "./code-editor";
import { ConfirmDialog } from "./misc";
import { Loader2, RefreshCw, Play, Download, Table2 } from "lucide-react";
import { toast } from "sonner";
import { useT } from "@/lib/store";

export function DatabaseWorkspace({ engine, version, databases, ready, targetLabel, onLockChange }: { engine: DatabaseEngine; version: string; databases: string[]; ready: boolean; targetLabel: string; onLockChange: (locked: boolean) => void }) {
  const t = useT();
  const [database, setDatabase] = React.useState("");
  const [table, setTable] = React.useState("");
  const [search, setSearch] = React.useState("");
  const [offset, setOffset] = React.useState(0);
  const [sql, setSql] = React.useState("SELECT VERSION() AS version;");
  const [result, setResult] = React.useState<api.DatabaseGrid[] | null>(null);
  const [queryError, setQueryError] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [confirmSql, setConfirmSql] = React.useState<string | null>(null);
  const [edit, setEdit] = React.useState<{ row: (string | null)[]; column: string; value: string; isNull: boolean } | null>(null);
  const request = (action: api.DatabaseWorkspaceRequest["action"], extra: Partial<api.DatabaseWorkspaceRequest> = {}) => api.databaseWorkspace(engine, version, { database, table, action, ...extra });
  const tables = useQuery({ queryKey: ["db-tables", engine, version, database], queryFn: () => request("tables"), enabled: ready && !!database, retry: false });
  const schema = useQuery({ queryKey: ["db-schema", engine, version, database, table], queryFn: () => request("schema"), enabled: ready && !!database && !!table, retry: false });
  const data = useQuery({ queryKey: ["db-rows", engine, version, database, table, offset], queryFn: () => request("rows", { offset }), enabled: ready && !!database && !!table, retry: false });
  const columns = schema.data?.[0];
  const keyIndex = columns?.columns.indexOf("Key") ?? -1;
  const hasPrimary = columns?.rows.some((row) => row[keyIndex] === "PRI");
  const editable = ready && hasPrimary && !["mysql", "sys", "information_schema", "performance_schema"].includes(database);
  const grid = data.data?.[0];
  const locked = busy || confirmSql != null || edit != null;
  React.useEffect(() => { onLockChange(locked); return () => onLockChange(false); }, [locked, onLockChange]);
  const refresh = async () => { await Promise.all([tables.refetch(), ...(table ? [schema.refetch(), data.refetch()] : [])]); };
  const run = async () => {
    if (!confirmSql || busy || !ready) return;
    setBusy(true); setQueryError(""); setResult(null);
    try { setResult(await request("sql", { sql: confirmSql, confirmed: true })); setConfirmSql(null); await refresh(); }
    catch (error) { const e = normalizeError(error); setQueryError(`${e.message}\n${e.detail ?? e.hint ?? ""}`); setConfirmSql(null); }
    finally { setBusy(false); }
  };
  const saveCell = async () => {
    if (!edit || busy || !ready) return;
    setBusy(true);
    try { await request("update", { confirmed: true, original: edit.row, column: edit.column, value: edit.isNull ? null : edit.value }); setEdit(null); await data.refetch(); toast.success(t("dbWorkspace.saved")); }
    catch (error) { toastError(error); } finally { setBusy(false); }
  };
  return <section className="mb-6 rounded-2xl border border-border bg-card p-4">
    <div className="mb-4 flex flex-wrap items-center gap-3"><h2 className="flex items-center gap-2 font-medium"><Table2 className="h-4 w-4 text-primary" />{t("dbWorkspace.title")}</h2><span className="text-xs text-muted">{targetLabel}</span></div>
    <div className="mb-4 flex flex-wrap gap-2"><Select value={database} disabled={!ready || locked} onValueChange={(value) => { setDatabase(value); setTable(""); setOffset(0); setResult(null); setQueryError(""); }}><SelectTrigger className="w-full sm:w-64" aria-label={t("dbWorkspace.chooseDatabase")}><SelectValue placeholder={t("dbWorkspace.chooseDatabaseHint")} /></SelectTrigger><SelectContent>{databases.map((name) => <SelectItem key={name} value={name}>{name}</SelectItem>)}</SelectContent></Select><Button size="sm" variant="secondary" disabled={!database || !ready || busy || tables.isFetching || data.isFetching} onClick={() => void refresh()}><RefreshCw className="h-3.5 w-3.5" />{t("dbWorkspace.refresh")}</Button></div>
    {!ready ? <p className="text-sm text-muted">{t("dbWorkspace.startHint")}</p> : !database ? <p className="text-sm text-muted">{t("dbWorkspace.intro")}</p> : <div className="grid min-w-0 gap-4 lg:grid-cols-[200px_minmax(0,1fr)]">
      <aside className="min-w-0 space-y-2"><Input aria-label={t("dbWorkspace.searchTables")} placeholder={t("dbWorkspace.searchTables")} value={search} onChange={(event) => setSearch(event.target.value)} />{tables.isFetching && <Loader2 className="h-4 w-4 animate-spin" />}{tables.error && <ErrorText error={tables.error} />}<div className="max-h-80 space-y-1 overflow-auto">{tables.data?.[0]?.rows.filter((row) => row[0]?.toLowerCase().includes(search.toLowerCase())).map((row) => <button key={row[0]} disabled={locked} onClick={() => { setTable(row[0]!); setOffset(0); }} className={`block w-full truncate rounded-lg px-3 py-2 text-left text-xs transition-colors ${table === row[0] ? "bg-primary/10 text-primary" : "hover:bg-fill"}`} title={row[0] ?? ""}>{row[0]}</button>)}{tables.isSuccess && !tables.data?.[0]?.rows.length && <p className="p-3 text-xs text-muted">{t("dbWorkspace.noTables")}</p>}</div></aside>
      <Tabs defaultValue="data" className="min-w-0"><TabsList><TabsTrigger value="data">{t("dbWorkspace.data")}</TabsTrigger><TabsTrigger value="schema">{t("dbWorkspace.schema")}</TabsTrigger><TabsTrigger value="sql">{t("dbWorkspace.sql")}</TabsTrigger></TabsList>
        <TabsContent value="data" className="space-y-3">{!table ? <p className="py-6 text-sm text-muted">{t("dbWorkspace.chooseTable")}</p> : <><div className="flex flex-wrap items-center justify-between gap-2 text-xs"><span className="font-mono">{database}.{table}</span><span className="text-muted">{editable ? t("dbWorkspace.editHint") : t("dbWorkspace.readonly")}</span></div>{data.isFetching && <p className="text-xs text-muted">{t("dbWorkspace.loading")}</p>}{data.error && <ErrorText error={data.error} />}{grid && <ResultGrid grid={{ ...grid, rows: grid.rows.slice(0, 100) }} onEdit={editable ? (row, column, value) => setEdit({ row, column, value: value ?? "", isNull: value === null }) : undefined} t={t} />}<div className="flex items-center gap-2"><Button size="sm" variant="secondary" disabled={!offset || data.isFetching || locked} onClick={() => setOffset(Math.max(0, offset - 100))}>{t("dbWorkspace.previous")}</Button><span className="text-xs">{t("dbWorkspace.pageInfo").replace("{page}", String(Math.floor(offset / 100) + 1))}</span><Button size="sm" variant="secondary" disabled={!grid || grid.rows.length <= 100 || data.isFetching || locked} onClick={() => setOffset(offset + 100)}>{t("dbWorkspace.next")}</Button></div></>}</TabsContent>
        <TabsContent value="schema" className="space-y-3">{!table ? <p className="py-6 text-sm text-muted">{t("dbWorkspace.chooseTable")}</p> : schema.isFetching ? <p className="text-sm text-muted">{t("dbWorkspace.readingSchema")}</p> : schema.error ? <ErrorText error={schema.error} /> : schema.data?.map((grid, index) => <div key={index}><h3 className="mb-2 text-xs text-muted">{index === 0 ? t("dbWorkspace.fields") : t("dbWorkspace.indexes")}</h3><ResultGrid grid={grid} t={t} /></div>)}</TabsContent>
        <TabsContent value="sql" className="space-y-3"><CodeEditor label={t("dbWorkspace.sqlEditor")} language="sql" value={sql} onChange={setSql} readOnly={busy} height="240px" /><div className="flex flex-wrap items-center gap-3"><Button disabled={!sql.trim() || busy || !ready} onClick={() => setConfirmSql(sql)}>{busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}{t("dbWorkspace.executeSql")}</Button><span className="text-xs text-muted">{t("dbWorkspace.connectionHint").replace("{database}", database)}</span></div>{queryError && <p role="alert" className="whitespace-pre-wrap break-words rounded-lg bg-error/5 p-3 text-xs text-error">{queryError}</p>}{result && (result.length ? result.map((grid, index) => <ResultGrid key={index} grid={grid} t={t} />) : <p role="status" className="text-sm text-running">{t("dbWorkspace.noRows")}</p>)}</TabsContent>
      </Tabs>
    </div>}
    <ConfirmDialog open={confirmSql != null} onOpenChange={(open) => { if (!open && !busy) setConfirmSql(null); }} title={t("dbWorkspace.confirmSql")} description={t("dbWorkspace.confirmSqlDesc").replace("{target}", targetLabel).replace("{database}", database)} confirmText={t("dbWorkspace.execute")} loading={busy} confirmDisabled={!ready} onConfirm={run}><pre className="max-h-60 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-fill p-3 text-xs">{confirmSql}</pre></ConfirmDialog>
    <ConfirmDialog open={!!edit} onOpenChange={(open) => { if (!open && !busy) setEdit(null); }} title={t("dbWorkspace.editTitle").replace("{column}", edit?.column ?? "")} description={t("dbWorkspace.editDesc").replace("{target}", targetLabel).replace("{database}", database).replace("{table}", table)} confirmText={t("dbWorkspace.saveEdit")} loading={busy} confirmDisabled={!ready} onConfirm={saveCell}>{edit && <div className="space-y-3"><div className="flex items-center gap-2"><Switch checked={edit.isNull} onCheckedChange={(isNull) => setEdit({ ...edit, isNull })} aria-label={t("dbWorkspace.setNull")} disabled={busy} /><span className="text-xs">{t("dbWorkspace.setNullHint")}</span></div><textarea className="min-h-28 w-full rounded-lg border border-border bg-fill p-3 font-mono text-sm" aria-label={t("dbWorkspace.fieldValue")} value={edit.value} onChange={(event) => setEdit({ ...edit, value: event.target.value })} disabled={edit.isNull || busy} /></div>}</ConfirmDialog>
  </section>;
}

function ErrorText({ error }: { error: unknown }) { const normalized = normalizeError(error); return <p role="alert" className="whitespace-pre-wrap break-words text-xs text-error">{normalized.message}{normalized.detail ? `\n${normalized.detail}` : ""}</p>; }

function ResultGrid({ grid, onEdit, t }: { grid: api.DatabaseGrid; onEdit?: (row: (string | null)[], column: string, value: string | null) => void; t: ReturnType<typeof useT> }) {
  const [page, setPage] = React.useState(0);
  React.useEffect(() => setPage(0), [grid.rows]);
  const exportCsv = () => {
    const cell = (value: string | null) => `"${(value == null ? "NULL" : /^[=+@\-\t\r]/.test(value) ? `'${value}` : value).replaceAll('"', '""')}"`;
    const blob = new Blob(["\ufeff", [grid.columns, ...grid.rows].map((row) => row.map(cell).join(",")).join("\r\n")], { type: "text/csv;charset=utf-8" });
    const url = URL.createObjectURL(blob); const link = document.createElement("a"); link.href = url; link.download = "query-result.csv"; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return <div className="min-w-0 space-y-2"><div className="flex items-center gap-2 text-xs text-muted"><span>{t("dbWorkspace.rowCount").replace("{count}", String(grid.rows.length))}</span><Button size="sm" variant="ghost" disabled={!grid.columns.length} onClick={exportCsv}><Download className="h-3 w-3" />{t("dbWorkspace.exportCsv")}</Button></div><div className="max-h-96 overflow-auto rounded-lg border border-border"><table className="w-full border-collapse text-left text-xs"><thead className="sticky top-0 z-10 bg-card"><tr>{grid.columns.map((column, index) => <th key={index} className="whitespace-nowrap border-b border-border px-3 py-2 font-medium">{column}</th>)}</tr></thead><tbody>{grid.rows.slice(page * 100, (page + 1) * 100).map((row, index) => <tr key={index} className="border-b border-border/50 hover:bg-fill">{row.map((value, col) => <td key={col} className="max-w-80 px-3 py-2"><button type="button" disabled={!onEdit} className="block max-w-80 truncate text-left font-mono disabled:cursor-text" title={value ?? t("dbWorkspace.null")} onClick={() => onEdit?.(row, grid.columns[col], value)}>{value === null ? <span className="italic text-faint">{t("dbWorkspace.null")}</span> : value || <span className="text-faint">{t("dbWorkspace.emptyString")}</span>}</button></td>)}</tr>)}</tbody></table>{!grid.rows.length && <p className="p-4 text-xs text-muted">{t("dbWorkspace.noRows")}</p>}</div>{grid.rows.length > 100 && <div className="flex items-center gap-2"><Button size="sm" variant="ghost" disabled={!page} onClick={() => setPage(page - 1)}>{t("dbWorkspace.previous")}</Button><span className="text-xs">{page + 1} / {Math.ceil(grid.rows.length / 100)}</span><Button size="sm" variant="ghost" disabled={(page + 1) * 100 >= grid.rows.length} onClick={() => setPage(page + 1)}>{t("dbWorkspace.next")}</Button></div>}</div>;
}
