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
import { Loader2, RefreshCw, Table2, X } from "lucide-react";
import { toast } from "sonner";
import { useT, useUI } from "@/lib/store";
import { SqlWorkbench } from "./sql-workbench";
import { ResultGrid, quoteIdentifier, sqlValue } from "./database-result-grid";

export function DatabaseWorkspace({ engine, version, databases, ready, targetLabel, onLockChange }: { engine: DatabaseEngine; version: string; databases: string[]; ready: boolean; targetLabel: string; onLockChange: (locked: boolean) => void }) {
  const t = useT();
  const english = useUI((state) => state.lang === "en");
  const text = (zh: string, en: string) => english ? en : zh;
  const [database, setDatabase] = React.useState("");
  const [table, setTable] = React.useState("");
  const [search, setSearch] = React.useState("");
  const [offset, setOffset] = React.useState(0);
  const [pageSize, setPageSize] = React.useState(100);
  const [mode, setMode] = React.useState("data");
  const [expanded, setExpanded] = React.useState(false);
  const [filters, setFilters] = React.useState<api.DatabaseFilter[]>([]);
  const [appliedFilters, setAppliedFilters] = React.useState<api.DatabaseFilter[]>([]);
  const [sortColumn, setSortColumn] = React.useState("");
  const [descending, setDescending] = React.useState(false);
  const [sqlLocked, setSqlLocked] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [edit, setEdit] = React.useState<{ row: (string | null)[]; column: string; value: string; isNull: boolean } | null>(null);
  const saveGuard = React.useRef(false);
  const request = (action: api.DatabaseWorkspaceRequest["action"], extra: Partial<api.DatabaseWorkspaceRequest> = {}) => api.databaseWorkspace(engine, version, { database, table, action, ...extra });
  const tables = useQuery({ queryKey: ["db-tables", engine, version, database], queryFn: () => request("tables"), enabled: ready && !!database, retry: false });
  const schema = useQuery({ queryKey: ["db-schema", engine, version, database, table], queryFn: () => request("schema"), enabled: ready && !!database && !!table, retry: false });
  const data = useQuery({ queryKey: ["db-rows", engine, version, database, table, offset, pageSize, appliedFilters, sortColumn, descending], queryFn: () => request("rows", { offset, pageSize, filters: appliedFilters, sortColumn: sortColumn || undefined, descending }), enabled: ready && !!database && !!table && mode === "data", retry: false });
  const columns = schema.data?.[0];
  const keyIndex = columns?.columns.indexOf("Key") ?? -1;
  const hasPrimary = columns?.rows.some((row) => row[keyIndex] === "PRI");
  const textSafe = columns?.rows.every((row) => {
    const type = row[columns.columns.indexOf("Type")]?.toLowerCase().split("(")[0] ?? "";
    const extra = row[columns.columns.indexOf("Extra")]?.toLowerCase() ?? "";
    return !["blob", "tinyblob", "mediumblob", "longblob", "binary", "varbinary", "bit", "geometry", "geometrycollection", "point", "multipoint", "polygon", "multipolygon", "linestring", "multilinestring"].includes(type) && !extra.includes("generated");
  });
  const fields = React.useMemo(() => columns?.rows.map((row) => row[columns.columns.indexOf("Field")]).filter((field): field is string => !!field) ?? [], [columns]);
  const completionSchema = React.useMemo(() => Object.fromEntries((tables.data?.[0]?.rows ?? []).filter((row) => row[0]).map((row) => [row[0]!, row[0] === table ? fields : []])), [tables.data, fields, table]);
  const editable = ready && hasPrimary && textSafe && tables.data?.[0]?.rows.find((row) => row[0] === table)?.[1] === "BASE TABLE" && !["mysql", "sys", "information_schema", "performance_schema"].includes(database.toLowerCase());
  const grid = React.useMemo(() => data.data?.[0] ? { ...data.data[0], rows: data.data[0].rows.slice(0, pageSize) } : undefined, [data.data, pageSize]);
  const locked = busy || sqlLocked || edit != null;
  const resetTable = (value = "") => { setTable(value); setOffset(0); setFilters([]); setAppliedFilters([]); setSortColumn(""); setDescending(false); };
  React.useEffect(() => { onLockChange(locked); return () => onLockChange(false); }, [locked, onLockChange]);
  const refresh = async () => { await Promise.all([tables.refetch(), ...(table ? [schema.refetch(), ...(mode === "data" ? [data.refetch()] : [])] : [])]); };
  const visibleTables = React.useMemo(() => {
    const needle = search.trim().toLocaleLowerCase();
    const entries = tables.data?.[0]?.rows ?? [];
    return needle ? entries.filter((row) => row[0]?.toLocaleLowerCase().includes(needle)) : entries;
  }, [search, tables.data]);
  const saveCell = async () => {
    if (!edit || saveGuard.current || !ready) return;
    saveGuard.current = true; setBusy(true);
    try { await request("update", { confirmed: true, original: edit.row, column: edit.column, value: edit.isNull ? null : edit.value }); setEdit(null); await data.refetch(); toast.success(t("dbWorkspace.saved")); }
    catch (error) { toastError(error); } finally { saveGuard.current = false; setBusy(false); }
  };
  return <section className={`mb-6 min-w-0 rounded-2xl border border-border bg-card p-4 ${expanded ? "fixed inset-3 z-40 overflow-auto shadow-xl" : ""}`}>
    <div className="mb-4 flex flex-wrap items-center gap-3"><h2 className="flex items-center gap-2 font-medium"><Table2 className="h-4 w-4 text-primary" />{t("dbWorkspace.title")}</h2><span className="text-xs text-muted">{targetLabel}</span><Button size="sm" className="ml-auto" variant="ghost" disabled={locked} onClick={() => setExpanded(!expanded)}>{text(expanded ? "收起工作台" : "展开工作台", expanded ? "Collapse workspace" : "Expand workspace")}</Button></div>
    <div className="mb-4 flex flex-wrap gap-2"><Select value={database} disabled={!ready || locked} onValueChange={(value) => { setDatabase(value); resetTable(); setSearch(""); }}><SelectTrigger className="w-full sm:w-64" aria-label={t("dbWorkspace.chooseDatabase")}><SelectValue placeholder={t("dbWorkspace.chooseDatabaseHint")} /></SelectTrigger><SelectContent>{databases.map((name) => <SelectItem key={name} value={name}>{name}</SelectItem>)}</SelectContent></Select><Button size="sm" variant="secondary" disabled={!database || !ready || locked || tables.isFetching || data.isFetching} onClick={() => void refresh()}><RefreshCw className="h-3.5 w-3.5" />{t("dbWorkspace.refresh")}</Button></div>
    {!ready ? <p className="text-sm text-muted">{t("dbWorkspace.startHint")}</p> : !database ? <p className="text-sm text-muted">{t("dbWorkspace.intro")}</p> : <div className="grid min-w-0 gap-4 lg:grid-cols-[200px_minmax(0,1fr)]">
      <aside className="min-w-0 space-y-2"><div className="relative"><Input aria-label={t("dbWorkspace.searchTables")} placeholder={t("dbWorkspace.searchTables")} value={search} onChange={(event) => setSearch(event.target.value)} className="pr-9" />{search && <button type="button" aria-label={t("dbWorkspace.clearTableSearch")} className="absolute right-1.5 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-full text-faint hover:bg-fill hover:text-foreground focus-visible:outline-none" onClick={() => setSearch("")}><X className="h-3.5 w-3.5" /></button>}</div>{tables.isFetching && <Loader2 className="h-4 w-4 animate-spin" />}{tables.error && <ErrorText error={tables.error} />}<div className="max-h-80 space-y-1 overflow-auto">{visibleTables.map((row) => <button key={row[0]} disabled={locked} onClick={() => { resetTable(row[0]!); }} className={`block w-full truncate rounded-lg px-3 py-2 text-left text-xs transition-colors ${table === row[0] ? "bg-primary/10 text-primary" : "hover:bg-fill"}`} title={row[0] ?? ""}>{row[0]}</button>)}{tables.isSuccess && !tables.data?.[0]?.rows.length && <p className="p-3 text-xs text-muted">{t("dbWorkspace.noTables")}</p>}{tables.isSuccess && tables.data?.[0]?.rows.length && !visibleTables.length && <p role="status" className="p-3 text-xs text-muted">{t("dbWorkspace.noTableMatches")}</p>}</div></aside>
      <Tabs value={mode} onValueChange={setMode} className="min-w-0"><TabsList><TabsTrigger value="data">{t("dbWorkspace.data")}</TabsTrigger><TabsTrigger value="schema">{t("dbWorkspace.schema")}</TabsTrigger><TabsTrigger value="sql">{t("dbWorkspace.sql")}</TabsTrigger></TabsList>
        <TabsContent value="data" className="space-y-3">{!table ? <p className="py-6 text-sm text-muted">{t("dbWorkspace.chooseTable")}</p> : <>
          <div className="flex flex-wrap justify-between gap-2 text-xs"><span className="font-mono">{database}.{table}</span><span className="text-muted">{editable ? t("dbWorkspace.editHint") : t("dbWorkspace.readonly")}</span></div>
          <div className="space-y-2 rounded-lg bg-fill p-3">
            {filters.map((filter, index) => <div key={index} className="flex flex-wrap items-center gap-2"><Choice label="筛选字段" value={filter.column} options={fields.map((name) => [name, name])} disabled={locked} onChange={(column) => setFilters(filters.map((item, i) => i === index ? { ...item, column } : item))} /><Choice label="比较方式" value={filter.operator} options={[["eq", "="], ["ne", "≠"], ["contains", text("包含", "Contains")], ["gt", ">"], ["gte", "≥"], ["lt", "<"], ["lte", "≤"], ["null", "IS NULL"], ["not_null", "IS NOT NULL"]]} disabled={locked} onChange={(operator) => setFilters(filters.map((item, i) => i === index ? { ...item, operator: operator as api.DatabaseFilter["operator"] } : item))} />{!["null", "not_null"].includes(filter.operator) && <Input aria-label="筛选值" className="min-w-32 flex-1" value={filter.value ?? ""} maxLength={4096} disabled={locked} onChange={(event) => setFilters(filters.map((item, i) => i === index ? { ...item, value: event.target.value } : item))} />}<Button size="icon-sm" variant="ghost" disabled={locked} aria-label="删除条件" onClick={() => setFilters(filters.filter((_, i) => i !== index))}><X className="h-3.5 w-3.5" /></Button></div>)}
            <div className="flex flex-wrap items-center gap-2"><Button size="sm" variant="secondary" disabled={!fields.length || locked || filters.length >= 8} onClick={() => setFilters([...filters, { column: fields[0], operator: "contains", value: "" }])}>{text("添加筛选条件", "Add filter")}</Button><Button size="sm" disabled={locked || data.isFetching} onClick={() => { setAppliedFilters(filters.map((f) => ({ ...f }))); setOffset(0); }}>{text("应用筛选", "Apply filters")}</Button><Button size="sm" variant="ghost" disabled={locked || !filters.length && !appliedFilters.length} onClick={() => { setFilters([]); setAppliedFilters([]); setOffset(0); }}>{text("重置", "Reset")}</Button><span className="text-xs text-muted">{text("所有条件同时满足 · 点击表头排序", "Match all filters · Click headers to sort")}</span></div>
          </div>
          {data.isFetching && <p role="status" className="text-xs text-muted">{t("dbWorkspace.loading")}</p>}{data.error && <ErrorText error={data.error} />}
          {grid && <ResultGrid grid={grid} t={t} table={editable ? table : undefined} pageSize={pageSize} rowOffset={offset} onSort={locked ? undefined : (column) => { setDescending(sortColumn === column ? !descending : false); setSortColumn(column); setOffset(0); }} sortColumn={sortColumn} descending={descending} onEdit={editable && !locked && !data.isFetching ? (row, column, value) => setEdit({ row, column, value: value ?? "", isNull: value === null }) : undefined} />}
          <div className="flex flex-wrap items-center gap-2"><Button size="sm" variant="secondary" disabled={!offset || data.isFetching || locked} onClick={() => setOffset(Math.max(0, offset - pageSize))}>{t("dbWorkspace.previous")}</Button><span className="text-xs">{t("dbWorkspace.pageInfo").replace("{page}", String(Math.floor(offset / pageSize) + 1))}</span><Button size="sm" variant="secondary" disabled={!data.data?.[0] || data.data[0].rows.length <= pageSize || data.isFetching || locked} onClick={() => setOffset(offset + pageSize)}>{t("dbWorkspace.next")}</Button><Choice label="每页行数" value={String(pageSize)} options={[25, 50, 100, 250, 500].map((n) => [String(n), text(`${n} 行 / 页`, `${n} rows / page`)])} disabled={locked} onChange={(value) => { setPageSize(Number(value)); setOffset(0); }} /><span className="text-xs text-muted">{text("导出仅包含已加载页；无主键时翻页顺序可能变化。", "Exports contain the loaded page. Tables without primary keys may reorder between pages.")}</span></div>
        </>}</TabsContent>
        <TabsContent value="schema" className="space-y-4">{!table ? <p className="py-6 text-sm text-muted">{t("dbWorkspace.chooseTable")}</p> : schema.isFetching ? <p className="text-sm text-muted">{t("dbWorkspace.readingSchema")}</p> : schema.error ? <ErrorText error={schema.error} /> : schema.data?.map((item, index) => <div key={index}><h3 className="mb-2 text-sm font-medium">{[t("dbWorkspace.fields"), t("dbWorkspace.indexes"), text("建表 / 视图语句", "DDL"), text("外键关系", "Foreign keys")][index]}</h3>{index === 2 ? <div className="space-y-2"><Button size="sm" variant="secondary" onClick={() => void navigator.clipboard.writeText(item.rows[0]?.[1] ?? "").catch(toastError)}>{text("复制 DDL", "Copy DDL")}</Button><CodeEditor label="DDL" language="sql" value={item.rows[0]?.[1] ?? ""} onChange={() => {}} readOnly height="280px" /></div> : <ResultGrid grid={item} t={t} />}</div>)}</TabsContent>
        <TabsContent value="sql" forceMount hidden={mode !== "sql"} className="space-y-3"><SqlWorkbench key={`${engine}:${version}:${database}`} engine={engine} version={version} database={database} table={table} targetLabel={targetLabel} schema={completionSchema} ready={ready && !busy && !edit} onLockChange={setSqlLocked} /></TabsContent>
      </Tabs>
    </div>}
    <ConfirmDialog open={!!edit} onOpenChange={(open) => { if (!open && !busy) setEdit(null); }} title={t("dbWorkspace.editTitle").replace("{column}", edit?.column ?? "")} description={t("dbWorkspace.editDesc").replace("{target}", targetLabel).replace("{database}", database).replace("{table}", table)} confirmText={t("dbWorkspace.saveEdit")} loading={busy} confirmDisabled={!ready} onConfirm={saveCell}>{edit && <div className="space-y-3"><div className="flex items-center gap-2"><Switch checked={edit.isNull} onCheckedChange={(isNull) => setEdit({ ...edit, isNull })} aria-label={t("dbWorkspace.setNull")} disabled={busy} /><span className="text-xs">{t("dbWorkspace.setNullHint")}</span></div><textarea className="min-h-28 w-full rounded-lg border border-border bg-fill p-3 font-mono text-sm" aria-label={t("dbWorkspace.fieldValue")} value={edit.value} onChange={(event) => setEdit({ ...edit, value: event.target.value })} disabled={edit.isNull || busy} /><details><summary className="cursor-pointer text-xs">{text("查看更新 SQL（含并发修改保护）", "Review update SQL (with conflict protection)")}</summary><pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all text-xs">{`UPDATE ${quoteIdentifier(database)}.${quoteIdentifier(table)} SET ${quoteIdentifier(edit.column)} = ${sqlValue(edit.isNull ? null : edit.value)} WHERE ${grid?.columns.map((name, index) => `BINARY ${quoteIdentifier(name)} <=> BINARY ${sqlValue(edit.row[index])}`).join(" AND ")} LIMIT 1;`}</pre></details></div>}</ConfirmDialog>
  </section>;
}

function ErrorText({ error }: { error: unknown }) { const normalized = normalizeError(error); return <p role="alert" className="whitespace-pre-wrap break-words text-xs text-error">{normalized.message}{normalized.detail ? `\n${normalized.detail}` : ""}</p>; }


function Choice({ label, value, options, onChange, disabled }: { label: string; value: string; options: string[][]; onChange: (value: string) => void; disabled?: boolean }) {
  return <Select value={value} onValueChange={onChange} disabled={disabled}><SelectTrigger className="w-auto min-w-28 max-w-64" aria-label={label}><SelectValue placeholder={label} /></SelectTrigger><SelectContent>{options.map(([key, name]) => <SelectItem key={key} value={key}>{name}</SelectItem>)}</SelectContent></Select>;
}
