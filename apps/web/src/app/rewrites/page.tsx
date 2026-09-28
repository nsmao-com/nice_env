"use client";
import * as React from "react";
import type { CustomRewrite } from "@nsb/schema";
import { PageHeader } from "@/components/layout/app-shell";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/shared/misc";
import { CodeEditor } from "@/components/shared/code-editor";
import { useSettings, useInvalidate, toastError } from "@/lib/hooks";
import { REWRITE_SNIPPETS } from "@/lib/rewrite-templates";
import * as api from "@/lib/api";
import { Plus, Copy, Pencil, Trash2 } from "lucide-react";
import { toast } from "sonner";

export default function Page() {
  const settings = useSettings();
  const invalidate = useInvalidate();
  const templates = settings.data?.rewriteTemplates ?? [];
  const [query, setQuery] = React.useState("");
  const [draft, setDraft] = React.useState<CustomRewrite | null>(null);
  const [editing, setEditing] = React.useState<number | null>(null);
  const [remove, setRemove] = React.useState<number | null>(null);
  const [discard, setDiscard] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const persist = async (next: CustomRewrite[]) => { setBusy(true); try { await api.setSetting("rewriteTemplates", next); await invalidate("settings"); setDraft(null); setRemove(null); toast.success("模板已保存"); } catch (error) { toastError(error); } finally { setBusy(false); } };
  const save = () => {
    if (!draft?.name.trim() || !draft.content.trim()) return;
    if (templates.some((item, index) => index !== editing && item.server === draft.server && item.name === draft.name.trim())) { toast.error("此服务器下已有同名模板"); return; }
    void persist(editing == null ? [...templates, { ...draft, name: draft.name.trim() }] : templates.map((item, index) => index === editing ? { ...draft, name: draft.name.trim() } : item));
  };
  const builtins: CustomRewrite[] = Object.entries(REWRITE_SNIPPETS).map(([name, content]) => ({ name, server: "nginx", content }));
  return <div className="pb-8"><PageHeader title="伪静态模板" subtitle="管理可重复使用的 Nginx / Apache 规则，在创建或编辑站点时选择。" />
    <div className="mb-5 flex flex-wrap gap-3"><Input className="max-w-sm" placeholder="搜索模板名称或服务器" aria-label="搜索模板" value={query} onChange={(event) => setQuery(event.target.value)} /><Button disabled={!settings.isSuccess} onClick={() => { setEditing(null); setDraft({ name: "", server: "nginx", content: "location / {\n    try_files $uri $uri/ /index.php?$query_string;\n}\n" }); }}><Plus className="h-4 w-4" />新增模板</Button></div>
    {settings.isError && <p role="alert" className="mb-4 text-error">模板读取失败 <Button variant="ghost" onClick={() => void settings.refetch()}>重试</Button></p>}
    <div className="grid gap-3 lg:grid-cols-2">{[...templates, ...builtins].map((item, index) => ({ item, index })).filter(({ item }) => `${item.name} ${item.server}`.toLowerCase().includes(query.toLowerCase())).map(({ item, index }) => <article key={index} className="rounded-xl border border-border bg-surface p-4">
      <div className="flex flex-wrap items-center gap-2"><h2 className="min-w-0 flex-1 font-medium">{item.name}</h2><span className="text-xs text-muted">{item.server} · {index < templates.length ? "自定义" : "内置"}</span><Button size="icon-sm" variant="ghost" aria-label={`复制 ${item.name}`} onClick={() => { setEditing(null); setDraft({ ...item, name: `${item.name} 副本` }); }}><Copy className="h-3.5 w-3.5" /></Button>{index < templates.length && <><Button size="icon-sm" variant="ghost" aria-label={`编辑 ${item.name}`} onClick={() => { setEditing(index); setDraft({ ...item }); }}><Pencil className="h-3.5 w-3.5" /></Button><Button size="icon-sm" variant="ghost" className="text-error" aria-label={`删除 ${item.name}`} onClick={() => setRemove(index)}><Trash2 className="h-3.5 w-3.5" /></Button></>}</div>
      <pre className="mt-3 max-h-32 overflow-auto whitespace-pre-wrap font-mono text-xs text-muted">{item.content}</pre>
    </article>)}</div>
    <Dialog open={!!draft} onOpenChange={(open) => { if (!open && !busy) setDiscard(true); }}><DialogContent className="max-h-[90dvh] max-w-3xl overflow-y-auto"><DialogHeader><DialogTitle>{editing == null ? "新增模板" : "编辑模板"}</DialogTitle></DialogHeader>{draft && <><Input aria-label="模板名称" placeholder="模板名称" maxLength={80} value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} disabled={busy} /><Select value={draft.server} disabled={busy} onValueChange={(server: "nginx" | "apache") => setDraft({ ...draft, server })}><SelectTrigger aria-label="服务器"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="nginx">Nginx</SelectItem><SelectItem value="apache">Apache</SelectItem></SelectContent></Select><p className="text-xs text-muted">{draft.server === "nginx" ? "填写 server 内部的 location / rewrite 规则，无需包裹 server 块。" : "填写 RewriteEngine、RewriteCond、RewriteRule 或 RewriteBase 指令。"}</p><CodeEditor label="模板规则" language={draft.server} value={draft.content} onChange={(content) => setDraft({ ...draft, content })} readOnly={busy} /><Button disabled={busy || !draft.name.trim() || !draft.content.trim()} onClick={save}>{busy ? "保存中…" : "保存模板"}</Button></>}</DialogContent></Dialog>
    <ConfirmDialog open={discard} onOpenChange={setDiscard} title="放弃未保存的模板？" description="当前编辑内容尚未保存。" confirmText="放弃修改" onConfirm={() => { setDiscard(false); setDraft(null); }} />
    <ConfirmDialog open={remove != null} onOpenChange={(open) => { if (!open && !busy) setRemove(null); }} title="删除模板" description="已使用此模板的站点保留自己的规则副本，不会受到影响。" danger loading={busy} onConfirm={() => void persist(templates.filter((_, index) => index !== remove))} />
  </div>;
}
