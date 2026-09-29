"use client";

import * as React from "react";
import type { CustomRewrite } from "@nsb/schema";
import { PageHeader } from "@/components/layout/app-shell";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/shared/misc";
import { CodeEditor } from "@/components/shared/code-editor";
import { ServiceIcon } from "@/components/shared/service-icon";
import { useSettings, useInvalidate, toastError } from "@/lib/hooks";
import { REWRITE_TEMPLATES, type RewriteTemplate } from "@/lib/rewrite-templates";
import * as api from "@/lib/api";
import { Plus, Copy, Pencil, Trash2, Search, FolderOpen, ChevronDown, ExternalLink } from "lucide-react";
import { toast } from "sonner";

const SERVERS = { nginx: "Nginx", apache: "Apache", caddy: "Caddy" } as const;
const CATEGORIES = ["全部类型", "PHP 框架", "内容管理", "静态站点", "自定义"];

export default function Page() {
  const settings = useSettings();
  const invalidate = useInvalidate();
  const templates = settings.data?.rewriteTemplates ?? [];
  const [server, setServer] = React.useState<CustomRewrite["server"]>("nginx");
  const [category, setCategory] = React.useState("全部类型");
  const [query, setQuery] = React.useState("");
  const [draft, setDraft] = React.useState<CustomRewrite | null>(null);
  const [initialDraft, setInitialDraft] = React.useState("");
  const [editing, setEditing] = React.useState<number | null>(null);
  const [remove, setRemove] = React.useState<number | null>(null);
  const [discard, setDiscard] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const openDraft = (item: CustomRewrite, index: number | null = null) => {
    const next = { name: item.name, server: item.server, content: item.content };
    setEditing(index); setDraft(next); setInitialDraft(JSON.stringify(next));
  };
  const closeDraft = () => {
    if (busy) return;
    if (JSON.stringify(draft) !== initialDraft) setDiscard(true);
    else setDraft(null);
  };
  const persist = async (next: CustomRewrite[]) => {
    setBusy(true);
    try {
      await api.setSetting("rewriteTemplates", next); await invalidate("settings");
      setDraft(null); setRemove(null); toast.success("模板已保存");
    } catch (error) { toastError(error); } finally { setBusy(false); }
  };
  const save = () => {
    if (!draft?.name.trim() || !draft.content.trim() || !settings.isSuccess) return;
    if (templates.some((item, index) => index !== editing && item.server === draft.server && item.name === draft.name.trim())) {
      toast.error("此服务器下已有同名模板"); return;
    }
    const next = { ...draft, name: draft.name.trim() };
    void persist(editing == null ? [...templates, next] : templates.map((item, index) => index === editing ? next : item));
  };
  const copyName = (item: CustomRewrite) => {
    let name = `${item.name} 副本`, suffix = 2;
    while (templates.some(entry => entry.server === item.server && entry.name === name)) name = `${item.name} 副本 ${suffix++}`;
    return name;
  };
  const entries: { item: CustomRewrite; index: number | null; builtin?: RewriteTemplate }[] = [
    ...templates.map((item, index) => ({ item, index })),
    ...REWRITE_TEMPLATES.map(item => ({ item, index: null, builtin: item })),
  ];
  const normalizedQuery = query.trim().toLowerCase();
  const filtered = entries.filter(({ item, builtin }) => item.server === server
    && (category === "全部类型" || category === (builtin?.category ?? "自定义"))
    && `${item.name} ${builtin?.description ?? ""} ${builtin?.documentRoot ?? ""}`.toLowerCase().includes(normalizedQuery));

  return <div className="pb-8">
    <PageHeader title="伪静态模板" subtitle={`${REWRITE_TEMPLATES.length / 3} 种应用与站点类型，提供 Nginx / Apache / Caddy 对应规则。也可以复制修改，保存自己的模板。`} />
    <div className="mb-5 space-y-4 rounded-2xl border border-border bg-surface p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap gap-1 rounded-xl bg-fill p-1" role="group" aria-label="服务器筛选">
          {Object.entries(SERVERS).map(([key, name]) => <Button key={key} variant={server === key ? "tinted" : "ghost"} aria-pressed={server === key} onClick={() => setServer(key as CustomRewrite["server"])}>
            <ServiceIcon id={key} className="h-4 w-4" />{name}<span className="text-xs opacity-65">{entries.filter(({ item }) => item.server === key).length}</span>
          </Button>)}
        </div>
        <Button disabled={!settings.isSuccess} onClick={() => openDraft({ name: "", server, content: REWRITE_TEMPLATES.find(item => item.server === server)!.content })}><Plus className="h-4 w-4" />新增模板</Button>
      </div>
      <div className="flex flex-wrap gap-3">
        <div className="relative min-w-0 flex-1 basis-64"><Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted" /><Input className="pl-9" placeholder="搜索应用、用途或运行目录…" aria-label="搜索模板" value={query} onChange={event => setQuery(event.target.value)} /></div>
        <Select value={category} onValueChange={setCategory}><SelectTrigger className="w-40" aria-label="模板类型"><SelectValue /></SelectTrigger><SelectContent>{CATEGORIES.map(name => <SelectItem key={name} value={name}>{name}</SelectItem>)}</SelectContent></Select>
      </div>
      <p className="text-xs leading-relaxed text-muted">规则按站点根目录部署。PHP 应用需选择 PHP 站点并设置对应公开目录；Node SSR 应用请使用反向代理。选择模板会为站点保存独立副本。</p>
    </div>
    {settings.isError && <p role="alert" className="mb-4 text-sm text-error">自定义模板读取失败，内置模板仍可浏览。<Button variant="ghost" onClick={() => void settings.refetch()}>重试</Button></p>}
    {settings.isPending && <p role="status" className="mb-4 text-sm text-muted">正在读取自定义模板…</p>}
    <p role="status" className="mb-3 text-xs text-muted">{SERVERS[server]} · {filtered.length} 个模板</p>
    {!filtered.length && <div className="rounded-2xl border border-dashed border-border p-10 text-center"><Search className="mx-auto mb-3 h-6 w-6 text-muted" /><p className="text-sm">{category === "自定义" && !normalizedQuery ? "还没有此服务器的自定义模板" : "没有找到匹配的模板"}</p><p className="mt-2 text-xs text-muted">可以调整关键词或类型，也可以从内置模板复制后修改。</p><Button variant="secondary" className="mt-4" onClick={() => { setQuery(""); setCategory("全部类型"); }}>查看全部类型</Button></div>}
    <div className="grid items-start gap-4 xl:grid-cols-2">
      {filtered.map(({ item, index, builtin }) => <article key={`${index == null ? "builtin" : index}:${item.server}:${item.name}`} className="min-w-0 overflow-hidden rounded-2xl border border-border bg-surface">
        <div className="space-y-3 p-5">
          <div className="flex items-start gap-3"><h2 className="min-w-0 flex-1 break-words text-base font-semibold">{item.name}</h2><span className="shrink-0 rounded-full bg-fill px-2.5 py-1 text-[11px] text-secondary">{builtin?.category ?? "自定义"}</span></div>
          <p className="text-sm leading-relaxed text-secondary">{builtin?.description ?? "你保存的站点重写规则，可在同类型服务器的站点中选择。"}</p>
          {builtin && <div className="flex items-start gap-2 text-xs text-muted"><FolderOpen className="h-4 w-4 shrink-0" /><span>运行目录：<code className="break-words text-secondary">{builtin.documentRoot}</code></span></div>}
          <div className="flex flex-wrap items-center gap-1.5 pt-1">
            <Button size="sm" variant="secondary" disabled={!settings.isSuccess} onClick={() => openDraft({ ...item, name: copyName(item) })}><Copy className="h-3.5 w-3.5" />复制并修改</Button>
            {index != null && <><Button size="sm" variant="ghost" disabled={!settings.isSuccess} onClick={() => openDraft(item, index)}><Pencil className="h-3.5 w-3.5" />编辑</Button><Button size="icon-sm" variant="ghost" disabled={busy} className="ml-auto text-error" aria-label={`删除 ${item.name}`} onClick={() => setRemove(index)}><Trash2 className="h-3.5 w-3.5" /></Button></>}
            {builtin && <a href={builtin.source} target="_blank" rel="noreferrer" className="ml-auto inline-flex items-center gap-1 rounded-full px-2 py-2 text-xs text-muted hover:text-primary focus-visible:outline-primary">部署说明<ExternalLink className="h-3 w-3" /></a>}
          </div>
        </div>
        <details className="group border-t border-border"><summary className="flex cursor-pointer list-none items-center justify-between bg-fill/40 px-5 py-3 text-xs text-secondary hover:bg-fill [&::-webkit-details-marker]:hidden">查看 {SERVERS[item.server]} 规则<ChevronDown className="h-3.5 w-3.5 transition-transform group-open:rotate-180" /></summary><pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words p-5 font-mono text-xs leading-relaxed text-secondary">{item.content}</pre></details>
      </article>)}
    </div>
    <Dialog open={!!draft} onOpenChange={open => { if (!open) closeDraft(); }}><DialogContent className="max-h-[90dvh] max-w-4xl overflow-y-auto"><DialogHeader><DialogTitle>{editing == null ? "新增模板" : "编辑模板"}</DialogTitle><DialogDescription>保存后可在站点的模板库中选择，已有站点的规则副本保持不变。</DialogDescription></DialogHeader>{draft && <>
      <div className="grid gap-4 sm:grid-cols-[1fr_180px]"><div className="space-y-2"><Label htmlFor="rewrite-name">模板名称</Label><Input id="rewrite-name" placeholder="例如：我的博客" maxLength={80} value={draft.name} onChange={event => setDraft({ ...draft, name: event.target.value })} disabled={busy} /></div><div className="space-y-2"><Label htmlFor="rewrite-server">服务器</Label><Select value={draft.server} disabled={busy} onValueChange={(value: CustomRewrite["server"]) => setDraft({ ...draft, server: value })}><SelectTrigger id="rewrite-server"><SelectValue /></SelectTrigger><SelectContent>{Object.entries(SERVERS).map(([key, name]) => <SelectItem key={key} value={key}>{name}</SelectItem>)}</SelectContent></Select></div></div>
      <p className="text-xs leading-relaxed text-muted">{draft.server === "caddy" ? "填写站点内的 try_files、rewrite 或 handle 规则，无需包裹域名块。" : draft.server === "nginx" ? "填写 server 内部的 location / rewrite 规则，无需包裹 server 块。" : "填写 RewriteEngine、RewriteCond、RewriteRule 或 RewriteBase 指令。"}切换服务器不会自动转换已输入的规则。</p>
      <CodeEditor label="模板规则" language={draft.server} value={draft.content} onChange={content => setDraft({ ...draft, content })} readOnly={busy} height="min(45dvh, 420px)" />
      <div className="flex justify-end gap-2"><Button variant="ghost" disabled={busy} onClick={closeDraft}>取消</Button><Button disabled={busy || !settings.isSuccess || !draft.name.trim() || !draft.content.trim()} onClick={save}>{busy ? "保存中…" : "保存模板"}</Button></div>
    </>}</DialogContent></Dialog>
    <ConfirmDialog open={discard} onOpenChange={setDiscard} title="放弃未保存的模板？" description="当前编辑内容尚未保存。" confirmText="放弃修改" onConfirm={() => { setDiscard(false); setDraft(null); }} />
    <ConfirmDialog open={remove != null} onOpenChange={open => { if (!open && !busy) setRemove(null); }} title="删除模板" description="已使用此模板的站点保留自己的规则副本，不会受到影响。" danger loading={busy} onConfirm={() => void persist(templates.filter((_, index) => index !== remove))} />
  </div>;
}
