"use client";
import * as React from "react";
import type { DatabaseEngine } from "@nsb/schema";
import { Plus, X, Play, Save, Loader2 } from "lucide-react";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { toastError } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { CodeEditor, type CodeEditorHandle } from "./code-editor";
import { ConfirmDialog } from "./misc";
import { ResultGrid, quoteIdentifier } from "./database-result-grid";
import { useT, useUI } from "@/lib/store";
import { useQueryClient } from "@tanstack/react-query";

type QueryTab = { id: string; name: string; sql: string };
type QueryRecord = { sql: string; at: number; elapsed: number; ok: boolean };
type QueryResult = { grids?: api.DatabaseGrid[]; error?: string; elapsed: number; sql: string };
type QuerySession = { tabs: QueryTab[]; active: string; snippets: QueryTab[]; history: QueryRecord[] };
// Bounded session memory keeps drafts when switching databases without writing SQL to disk.
const sessions = new Map<string, QuerySession>();
const newTab = (sql = "SELECT VERSION() AS version;", name = "SQL") => ({ id: crypto.randomUUID(), name, sql });
export function SqlWorkbench({ engine, version, database, table, targetLabel, schema, ready, onLockChange }: { engine: DatabaseEngine; version: string; database: string; table: string; targetLabel: string; schema: Record<string, string[]>; ready: boolean; onLockChange: (locked: boolean) => void }) {
  const t = useT();
  const english = useUI((state) => state.lang === "en");
  const text = (zh: string, en: string) => english ? en : zh;

  const client = useQueryClient();
  const storageKey = `niceenv-sql-v1:${engine}:${version}:${database}`;
  const [tabs, setTabs] = React.useState<QueryTab[]>(() => [newTab()]);
  const [active, setActive] = React.useState("");
  const [history, setHistory] = React.useState<QueryRecord[]>([]);
  const [snippets, setSnippets] = React.useState<QueryTab[]>([]);
  const [results, setResults] = React.useState<Record<string, QueryResult>>({});
  const [remember, setRemember] = React.useState(false);
  const [loaded, setLoaded] = React.useState(false);
  const [storageError, setStorageError] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [confirm, setConfirm] = React.useState<string | null>(null);
  const [closing, setClosing] = React.useState<string | null>(null);
  const guard = React.useRef(false);
  const editor = React.useRef<CodeEditorHandle>(null);
  const current = tabs.find((tab) => tab.id === active) ?? tabs[0];
  const result = results[current.id];
  React.useEffect(() => {
    try {
      const raw = localStorage.getItem(storageKey);
      let restored: QuerySession | undefined;
      if (raw) {
        if (raw.length > 2_000_000) throw new Error("本地 SQL 记录过大，请清除后重试。");
        const saved = JSON.parse(raw);
        const validTab = (entry: unknown): entry is QueryTab => !!entry && typeof entry === "object" && "id" in entry && typeof entry.id === "string" && /^[0-9a-f-]{36}$/i.test(entry.id) && "name" in entry && typeof entry.name === "string" && entry.name.length <= 80 && "sql" in entry && typeof entry.sql === "string" && entry.sql.length <= 100_000;
        if (!saved || !Array.isArray(saved.tabs) || !saved.tabs.length || saved.tabs.length > 12 || !saved.tabs.every(validTab) || new Set(saved.tabs.map((tab: QueryTab) => tab.id)).size !== saved.tabs.length || !Array.isArray(saved.snippets) || saved.snippets.length > 30 || !saved.snippets.every(validTab) || !Array.isArray(saved.history) || saved.history.length > 50 || !saved.history.every((entry: QueryRecord) => entry && typeof entry.sql === "string" && entry.sql.length <= 100_000 && Number.isFinite(entry.at) && Number.isFinite(entry.elapsed) && typeof entry.ok === "boolean")) throw new Error("本地 SQL 记录无法读取，原记录已保留。");
        restored = saved;
      }
      const saved = sessions.get(storageKey) ?? restored;
      if (saved) { setTabs(saved.tabs); setSnippets(saved.snippets); setHistory(saved.history); setActive(saved.tabs.some((tab) => tab.id === saved.active) ? saved.active : saved.tabs[0].id); }
      setRemember(!!raw);
    } catch (error) { setStorageError(normalizeError(error).message); }
    setLoaded(true);
  }, [storageKey]);
  const snapshot = React.useRef<QuerySession>({ tabs, active: current.id, snippets, history });
  snapshot.current = { tabs, active: current.id, snippets, history };
  const serialize = () => {
    const state = snapshot.current;
    if ([...state.tabs, ...state.snippets, ...state.history].some((item) => item.sql.length > 100_000)) throw new Error("单条 SQL 超过 10 万字符，请缩短后重试保存。");
    const raw = JSON.stringify(state);
    if (raw.length > 2_000_000) throw new Error("SQL 记录超过 2 MB，请清除历史或删除查询后重试保存。");
    return raw;
  };
  React.useEffect(() => {
    if (!loaded || !remember || storageError) return;
    const persist = () => { try { localStorage.setItem(storageKey, serialize()); } catch (error) { setStorageError(normalizeError(error).message); } };
    const timer = setTimeout(persist, 500);
    window.addEventListener("pagehide", persist);
    return () => { clearTimeout(timer); window.removeEventListener("pagehide", persist); };
  }, [tabs, active, snippets, history, remember, loaded, storageKey, storageError]);
  const persistOnLeave = React.useRef<(() => void) | null>(null);
  persistOnLeave.current = loaded ? () => {
    sessions.delete(storageKey); sessions.set(storageKey, snapshot.current);
    while (sessions.size > 8) sessions.delete(sessions.keys().next().value!);
    if (remember && !storageError) { try { localStorage.setItem(storageKey, serialize()); } catch (error) { toastError(error); } }
  } : null;
  React.useEffect(() => () => persistOnLeave.current?.(), []);
  React.useEffect(() => { onLockChange(busy || confirm !== null || closing !== null); return () => onLockChange(false); }, [busy, confirm, closing, onLockChange]);
  const addTab = (sql?: string, name?: string) => { if (tabs.length >= 12) return; const tab = newTab(sql, name); setTabs([...tabs, tab]); setActive(tab.id); };
  const propose = React.useCallback((sql: string) => { if (ready && !busy && sql.trim()) setConfirm(sql); }, [ready, busy]);
  const execute = async () => {
    if (!confirm || !ready || guard.current) return;
    guard.current = true; setBusy(true); const sql = confirm; const id = current.id; const start = performance.now(); let ok = false;
    try { const grids = await api.databaseWorkspace(engine, version, { database, action: "sql", sql, confirmed: true }); ok = true; setResults((old) => ({ ...Object.fromEntries(Object.entries(old).filter(([key]) => key !== id).slice(-2)), [id]: { grids, elapsed: performance.now() - start, sql } })); }
    catch (error) { const e = normalizeError(error); setResults((old) => ({ ...old, [id]: { error: `${e.message}\n${e.detail ?? e.hint ?? ""}`, elapsed: performance.now() - start, sql } })); }
    finally {
      if (sql.length <= 100_000) setHistory((old) => [{ sql, at: Date.now(), elapsed: performance.now() - start, ok }, ...old].slice(0, 50));
      for (const key of ["db-tables", "db-schema", "db-rows"]) void client.invalidateQueries({ queryKey: [key, engine, version, database] });
      setConfirm(null); setBusy(false); guard.current = false;
    }
  };
  const locked = busy || confirm !== null || closing !== null;
  return <div className="space-y-3">
    <div className="flex flex-wrap items-center gap-1" role="group" aria-label={text("SQL 查询标签", "SQL query tabs")}>{tabs.map((tab) => <div key={tab.id} className={`flex items-center rounded-lg border ${current.id === tab.id ? "border-primary bg-primary/10" : "border-border"}`}><button aria-pressed={current.id === tab.id} className="max-w-40 truncate px-3 py-2 text-xs" disabled={locked} onClick={() => setActive(tab.id)}>{tab.name || "SQL"}</button>{tabs.length > 1 && <Button size="icon-sm" variant="ghost" disabled={locked} aria-label={`${text("关闭查询", "Close query")} ${tab.name}`} onClick={() => setClosing(tab.id)}><X className="h-3 w-3" /></Button>}</div>)}<Button size="icon-sm" variant="ghost" disabled={locked || tabs.length >= 12} aria-label={text("新建查询", "New query")} onClick={() => addTab("", `SQL ${tabs.length + 1}`)}><Plus className="h-4 w-4" /></Button></div>
    <div className="flex flex-wrap gap-2"><Input aria-label={text("查询名称", "Query name")} className="w-48" maxLength={80} disabled={locked} value={current.name} onChange={(event) => setTabs(tabs.map((tab) => tab.id === current.id ? { ...tab, name: event.target.value } : tab))} /><Button size="sm" variant="secondary" disabled={locked || !current.sql.trim() || snippets.length >= 30} onClick={() => setSnippets([...snippets, { ...current, id: crypto.randomUUID() }])}><Save className="h-3.5 w-3.5" />{text("收藏 SQL", "Save snippet")}</Button><Button size="sm" variant="ghost" disabled={locked || !table || tabs.length >= 12} onClick={() => addTab(`SELECT * FROM ${quoteIdentifier(database)}.${quoteIdentifier(table)} LIMIT 100;`, table.slice(0, 80))}>{text("查询当前表", "Query selected table")}</Button></div>
    <CodeEditor key={current.id} ref={editor} label={t("dbWorkspace.sqlEditor")} language="sql" value={current.sql} onChange={(sql) => setTabs(tabs.map((tab) => tab.id === current.id ? { ...tab, sql } : tab))} readOnly={locked} height="280px" sqlSchema={schema} sqlTable={table || undefined} onExecute={propose} />
    <div className="flex flex-wrap items-center gap-3"><Button disabled={!current.sql.trim() || locked || !ready} onClick={() => propose(editor.current?.selectedText() || current.sql)}>{busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}{text("运行选中 / 全部 SQL", "Run selection / all SQL")}</Button><span className="text-xs text-muted">Ctrl / ⌘ Enter · {database} · {text("最长 30 秒 · 保留最近 3 个标签的结果", "30 second limit · Results retained for 3 tabs")}</span></div>
    <details className="rounded-lg border border-border p-3"><summary className="cursor-pointer text-xs">{text("查询记录与收藏", "History and saved queries")} · {history.length} / {snippets.length}</summary><div className="mt-3 space-y-3"><label className="flex items-center gap-2 text-xs"><Switch checked={remember} disabled={!loaded || !!storageError} onCheckedChange={(checked) => { setRemember(checked); if (!checked) { try { localStorage.removeItem(storageKey); } catch (error) { setStorageError(normalizeError(error).message); } } }} />{text("在本机保存查询标签、历史和收藏（SQL 可能包含敏感数据）", "Store query tabs, history and snippets locally (SQL may contain sensitive values)")}</label>{storageError && <p role="alert" className="text-xs text-error">{storageError}</p>}<Button size="sm" variant="ghost" onClick={() => { setRemember(false); setHistory([]); setSnippets([]); try { localStorage.removeItem(storageKey); setStorageError(""); } catch (error) { setStorageError(normalizeError(error).message); } }}>{text("清除历史、收藏和本机记录", "Clear history, snippets and local records")}</Button><div className="grid gap-3 xl:grid-cols-2"><div><h4 className="mb-2 text-xs font-medium">{text("收藏", "Saved queries")}</h4>{!snippets.length && <p className="text-xs text-muted">{text("还没有收藏的 SQL。", "No saved queries yet.")}</p>}{snippets.map((item) => <div key={item.id} className="flex items-center gap-1"><Button className="min-w-0 flex-1 justify-start truncate" size="sm" variant="ghost" disabled={locked || tabs.length >= 12} onClick={() => addTab(item.sql, item.name)}>{item.name || "SQL"}</Button><Button size="icon-sm" variant="ghost" aria-label={text("删除收藏", "Delete saved query")} onClick={() => setSnippets(snippets.filter((entry) => entry.id !== item.id))}><X className="h-3 w-3" /></Button></div>)}</div><div className="max-h-64 overflow-auto"><h4 className="mb-2 text-xs font-medium">{text("最近 50 次执行", "Last 50 executions")}</h4>{!history.length && <p className="text-xs text-muted">{text("执行后将在这里保留 SQL 和耗时。", "Executed SQL and timings will appear here.")}</p>}{history.map((item, index) => <button key={`${item.at}:${index}`} disabled={locked || tabs.length >= 12} className="mb-1 block w-full rounded-lg p-2 text-left hover:bg-fill" title={item.sql} onClick={() => addTab(item.sql, "历史查询")}><span className="block text-[10px] text-muted">{item.ok ? "✓" : "✕"} {new Date(item.at).toLocaleString()} · {Math.round(item.elapsed)} ms</span><span className="block truncate font-mono text-xs">{item.sql}</span></button>)}</div></div></div></details>
    {result && <div className="space-y-3"><p className="text-xs text-muted">{Math.round(result.elapsed)} ms · {text("上次执行结果；编辑 SQL 不会自动重新执行", "Last execution result; editing SQL does not rerun it")}</p><details><summary className="cursor-pointer text-xs text-muted">{text("查看本次执行的 SQL", "View executed SQL")}</summary><pre className="max-h-40 overflow-auto whitespace-pre-wrap text-xs">{result.sql}</pre></details>{result.error && <p role="alert" className="whitespace-pre-wrap break-words rounded-lg bg-error/5 p-3 text-xs text-error">{result.error}</p>}{result.grids?.map((grid, index) => <div key={index}><h4 className="text-xs font-medium">{text("结果集", "Result set")} {index + 1}</h4><ResultGrid grid={grid} t={t} /></div>)}{result.grids?.length === 0 && <p role="status" className="text-sm text-running">{t("dbWorkspace.noRows")}</p>}</div>}
    <ConfirmDialog open={confirm !== null} onOpenChange={(open) => { if (!open && !busy) setConfirm(null); }} title={t("dbWorkspace.confirmSql")} description={t("dbWorkspace.confirmSqlDesc").replace("{target}", targetLabel).replace("{database}", database)} confirmText={t("dbWorkspace.execute")} loading={busy} confirmDisabled={!ready} onConfirm={execute}><pre className="max-h-60 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-fill p-3 text-xs">{confirm}</pre></ConfirmDialog>
    <ConfirmDialog open={closing !== null} onOpenChange={(open) => { if (!open) setClosing(null); }} title={text("关闭查询标签？", "Close query tab?")} description={text("此标签的草稿和结果将移除。需要保留 SQL 时，请先取消并收藏。", "The draft and results will be removed. Cancel and save a snippet to keep this SQL.")} onConfirm={() => { setTabs(tabs.filter((tab) => tab.id !== closing)); setResults((old) => { const next = { ...old }; if (closing) delete next[closing]; return next; }); setClosing(null); }} />
  </div>;
}
