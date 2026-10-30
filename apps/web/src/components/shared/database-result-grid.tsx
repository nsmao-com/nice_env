"use client";
import * as React from "react";
import type { DatabaseGrid } from "@/lib/api";
import { useT, useUI } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { X, Copy, Download } from "lucide-react";

export const quoteIdentifier = (name: string) => `\`${name.replaceAll("`", "``")}\``;
export const sqlValue = (value: string | null) => value === null ? "NULL" : `CONVERT(X'${Array.from(new TextEncoder().encode(value), (byte) => byte.toString(16).padStart(2, "0")).join("")}' USING utf8mb4)`;
function download(content: string, name: string, type: string) {
  const url = URL.createObjectURL(new Blob([content], { type }));
  const link = document.createElement("a"); link.href = url; link.download = name; link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export function ResultGrid({ grid, onEdit, t, table, rowOffset = 0, pageSize = 100, onSort, sortColumn, descending }: { grid: DatabaseGrid; onEdit?: (row: (string | null)[], column: string, value: string | null) => void; t: ReturnType<typeof useT>; table?: string; rowOffset?: number; pageSize?: number; onSort?: (column: string) => void; sortColumn?: string; descending?: boolean }) {

  const english = useUI((state) => state.lang === "en");
  const text = (zh: string, en: string) => english ? en : zh;
  const [page, setPage] = React.useState(0);
  const [search, setSearch] = React.useState("");
  const [cell, setCell] = React.useState<{ row: (string | null)[]; column: string; value: string | null } | null>(null);
  const rows = React.useMemo(() => grid.rows.map((row, index) => ({ row, index })).filter(({ row }) => !search || row.some((value) => value?.toLocaleLowerCase().includes(search.toLocaleLowerCase()))), [grid.rows, search]);
  React.useEffect(() => setPage(0), [grid.rows, search]);
  React.useEffect(() => setCell(null), [grid]);
  const exportCsv = () => {
    const escaped = (value: string | null) => `"${(value == null ? "NULL" : /^[=+@\-\t\r]/.test(value) ? `'${value}` : value).replaceAll('"', '""')}"`;
    download("\ufeff" + [grid.columns, ...rows.map(({ row }) => row)].map((row) => row.map(escaped).join(",")).join("\r\n"), "query-result.csv", "text/csv;charset=utf-8");
  };
  return <div className="min-w-0 space-y-2">
    <div className="flex flex-wrap items-center gap-2 text-xs text-muted"><span>{rows.length} / {grid.rows.length} {text("行", "rows")}</span><Input className="ml-auto w-48" aria-label={text("搜索已加载结果", "Search loaded results")} placeholder={text("搜索已加载结果", "Search loaded results")} value={search} onChange={(event) => setSearch(event.target.value)} /><Button size="sm" variant="ghost" disabled={!grid.columns.length} onClick={exportCsv}><Download className="h-3 w-3" />CSV</Button><Button size="sm" variant="ghost" disabled={!grid.columns.length} onClick={() => download(JSON.stringify({ columns: grid.columns, rows: rows.map(({ row }) => row) }, null, 2), "query-result.json", "application/json")}>JSON</Button>{table && <Button size="sm" variant="ghost" disabled={!rows.length} onClick={() => download(rows.map(({ row }) => `INSERT INTO ${quoteIdentifier(table)} (${grid.columns.map(quoteIdentifier).join(", ")}) VALUES (${row.map(sqlValue).join(", ")});`).join("\n"), "query-result.sql", "text/plain;charset=utf-8")}>INSERT SQL</Button>}</div>
    <div className="max-h-[480px] overflow-auto rounded-lg border border-border"><table className="w-full border-collapse text-left text-xs"><thead className="sticky top-0 z-10 bg-card"><tr><th className="border-b border-border px-2 py-2 text-muted">#</th>{grid.columns.map((column, index) => <th key={index} className="border-b border-border font-medium"><div className="min-w-28 max-w-96 resize-x overflow-auto px-3 py-2"><button disabled={!onSort} onClick={() => onSort?.(column)} className="whitespace-nowrap text-left" title={column}>{column}{sortColumn === column ? descending ? " ↓" : " ↑" : ""}</button></div></th>)}</tr></thead><tbody>{rows.slice(page * pageSize, (page + 1) * pageSize).map(({ row, index }) => <tr key={index} className="border-b border-border/50 hover:bg-fill"><td className="px-2 py-2 text-faint">{rowOffset + index + 1}</td>{row.map((value, col) => <td key={col} className="max-w-80 px-3 py-2"><button type="button" className="block max-w-80 truncate text-left font-mono" title={text("点击查看完整值", "Inspect full value")} onClick={() => setCell({ row, column: grid.columns[col], value })}>{value === null ? <span className="italic text-faint">{t("dbWorkspace.null")}</span> : value || <span className="text-faint">{t("dbWorkspace.emptyString")}</span>}</button></td>)}</tr>)}</tbody></table>{!rows.length && <p className="p-4 text-xs text-muted">{search ? text("已加载结果中没有匹配项。", "No matches in loaded results.") : t("dbWorkspace.noRows")}</p>}</div>
    {rows.length > pageSize && <div className="flex items-center gap-2"><Button size="sm" variant="ghost" disabled={!page} onClick={() => setPage(page - 1)}>{t("dbWorkspace.previous")}</Button><span className="text-xs">{page + 1} / {Math.ceil(rows.length / pageSize)}</span><Button size="sm" variant="ghost" disabled={(page + 1) * pageSize >= rows.length} onClick={() => setPage(page + 1)}>{t("dbWorkspace.next")}</Button></div>}
    {cell && <div className="space-y-2 rounded-lg border border-border bg-fill p-3"><div className="flex items-center gap-2 text-xs"><strong>{cell.column}</strong><Button size="sm" variant="ghost" onClick={() => void navigator.clipboard.writeText(cell.value ?? "NULL").catch(toastError)}><Copy className="h-3 w-3" />{text("复制值", "Copy value")}</Button>{onEdit && <Button size="sm" variant="secondary" onClick={() => onEdit(cell.row, cell.column, cell.value)}>{text("编辑此单元格", "Edit this cell")}</Button>}<Button size="icon-sm" variant="ghost" className="ml-auto" aria-label={text("关闭详情", "Close detail")} onClick={() => setCell(null)}><X className="h-3 w-3" /></Button></div><pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all font-mono text-xs">{cell.value ?? "NULL"}</pre></div>}
  </div>;
}
