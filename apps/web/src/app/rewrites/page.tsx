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
import { useT } from "@/lib/store";
import { REWRITE_TEMPLATES, type RewriteTemplate } from "@/lib/rewrite-templates";
import * as api from "@/lib/api";
import { Plus, Copy, Pencil, Trash2, Search, FolderOpen, ChevronDown, ExternalLink } from "lucide-react";
import { toast } from "sonner";

const SERVERS = { nginx: "Nginx", apache: "Apache", caddy: "Caddy" } as const;
const CATEGORIES = [
  { value: "all", label: "rewrites.allCategories" },
  { value: "php", label: "rewrites.phpFramework" },
  { value: "cms", label: "rewrites.cms" },
  { value: "static", label: "rewrites.staticSites" },
  { value: "custom", label: "rewrites.custom" },
] as const;
type RewriteCategory = (typeof CATEGORIES)[number]["value"];

function categoryOf(item: RewriteTemplate | CustomRewrite): RewriteCategory {
  if (!("category" in item)) return "custom";
  return item.category === "PHP 框架" ? "php" : item.category === "内容管理" ? "cms" : "static";
}

function categoryLabelKey(category: RewriteCategory) {
  return CATEGORIES.find(item => item.value === category)!.label;
}

export default function Page() {
  const t = useT();
  const settings = useSettings();
  const invalidate = useInvalidate();
  const templates = settings.data?.rewriteTemplates ?? [];
  const [server, setServer] = React.useState<CustomRewrite["server"]>("nginx");
  const [category, setCategory] = React.useState<RewriteCategory>("all");
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
      setDraft(null); setRemove(null); toast.success(t("rewrites.saved"));
    } catch (error) { toastError(error); } finally { setBusy(false); }
  };
  const save = () => {
    if (!draft?.name.trim() || !draft.content.trim() || !settings.isSuccess) return;
    if (templates.some((item, index) => index !== editing && item.server === draft.server && item.name === draft.name.trim())) {
      toast.error(t("rewrites.duplicate")); return;
    }
    const next = { ...draft, name: draft.name.trim() };
    void persist(editing == null ? [...templates, next] : templates.map((item, index) => index === editing ? next : item));
  };
  const copyName = (item: CustomRewrite) => {
    let name = `${item.name} ${t("rewrites.copySuffix")}`, suffix = 2;
    while (templates.some(entry => entry.server === item.server && entry.name === name)) name = `${item.name} ${t("rewrites.copySuffix")} ${suffix++}`;
    return name;
  };
  const entries: { item: CustomRewrite; index: number | null; builtin?: RewriteTemplate }[] = [
    ...templates.map((item, index) => ({ item, index })),
    ...REWRITE_TEMPLATES.map(item => ({ item, index: null, builtin: item })),
  ];
  const normalizedQuery = query.trim().toLowerCase();
  const filtered = entries.filter(({ item, builtin }) => item.server === server
    && (category === "all" || category === categoryOf(builtin ?? item))
    && `${item.name} ${builtin?.description ?? ""} ${builtin?.documentRoot ?? ""}`.toLowerCase().includes(normalizedQuery));

  return <div className="pb-8">
    <PageHeader title={t("rewrites.title")} subtitle={t("rewrites.subtitle").replace("{count}", String(REWRITE_TEMPLATES.length / 3))} />
    <div className="mb-5 space-y-4 rounded-2xl border border-border bg-surface p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap gap-1 rounded-xl bg-fill p-1" role="group" aria-label={t("rewrites.serverFilter")}>
          {Object.entries(SERVERS).map(([key, name]) => <Button key={key} variant={server === key ? "tinted" : "ghost"} aria-pressed={server === key} onClick={() => setServer(key as CustomRewrite["server"])}>
            <ServiceIcon id={key} className="h-4 w-4" />{name}<span className="text-xs opacity-65">{entries.filter(({ item }) => item.server === key).length}</span>
          </Button>)}
        </div>
        <Button disabled={!settings.isSuccess} onClick={() => openDraft({ name: "", server, content: REWRITE_TEMPLATES.find(item => item.server === server)!.content })}><Plus className="h-4 w-4" />{t("rewrites.add")}</Button>
      </div>
      <div className="flex flex-wrap gap-3">
        <div className="relative min-w-0 flex-1 basis-64"><Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted" /><Input className="pl-9" placeholder={t("rewrites.searchPlaceholder")} aria-label={t("rewrites.searchAria")} value={query} onChange={event => setQuery(event.target.value)} /></div>
        <Select value={category} onValueChange={value => setCategory(value as RewriteCategory)}><SelectTrigger className="w-40" aria-label={t("rewrites.categoryFilter")}><SelectValue /></SelectTrigger><SelectContent>{CATEGORIES.map(item => <SelectItem key={item.value} value={item.value}>{t(item.label)}</SelectItem>)}</SelectContent></Select>
      </div>
      <p className="text-xs leading-relaxed text-muted">{t("rewrites.hint")}</p>
    </div>
    {settings.isError && <p role="alert" className="mb-4 text-sm text-error">{t("rewrites.loadFailed")} <Button variant="ghost" onClick={() => void settings.refetch()}>{t("common.retry")}</Button></p>}
    {settings.isPending && <p role="status" className="mb-4 text-sm text-muted">{t("rewrites.loading")}</p>}
    <p role="status" className="mb-3 text-xs text-muted">{SERVERS[server]} · {t("rewrites.count").replace("{count}", String(filtered.length))}</p>
    {!filtered.length && <div className="rounded-2xl border border-dashed border-border p-10 text-center"><Search className="mx-auto mb-3 h-6 w-6 text-muted" /><p className="text-sm">{category === "custom" && !normalizedQuery ? t("rewrites.noCustom") : t("rewrites.noMatches")}</p><p className="mt-2 text-xs text-muted">{t("rewrites.noMatchesHint")}</p><Button variant="secondary" className="mt-4" onClick={() => { setQuery(""); setCategory("all"); }}>{t("rewrites.clearFilters")}</Button></div>}
    <div className="grid items-start gap-4 xl:grid-cols-2">
      {filtered.map(({ item, index, builtin }) => <article key={`${index == null ? "builtin" : index}:${item.server}:${item.name}`} className="min-w-0 overflow-hidden rounded-2xl border border-border bg-surface">
        <div className="space-y-3 p-5">
          <div className="flex items-start gap-3"><h2 className="min-w-0 flex-1 break-words text-base font-semibold">{item.name}</h2><span className="shrink-0 rounded-full bg-fill px-2.5 py-1 text-[11px] text-secondary">{t(categoryLabelKey(categoryOf(builtin ?? item)))}</span></div>
          <p className="text-sm leading-relaxed text-secondary">{builtin?.description ?? t("rewrites.customDescription")}</p>
          {builtin && <div className="flex items-start gap-2 text-xs text-muted"><FolderOpen className="h-4 w-4 shrink-0" /><span>{t("rewrites.documentRoot")}：<code className="break-words text-secondary">{builtin.documentRoot}</code></span></div>}
          <div className="flex flex-wrap items-center gap-1.5 pt-1">
            <Button size="sm" variant="secondary" disabled={!settings.isSuccess} onClick={() => openDraft({ ...item, name: copyName(item) })}><Copy className="h-3.5 w-3.5" />{t("rewrites.copyEdit")}</Button>
            {index != null && <><Button size="sm" variant="ghost" disabled={!settings.isSuccess} onClick={() => openDraft(item, index)}><Pencil className="h-3.5 w-3.5" />{t("common.edit")}</Button><Button size="icon-sm" variant="ghost" disabled={busy} className="ml-auto text-error" aria-label={`${t("common.delete")} ${item.name}`} onClick={() => setRemove(index)}><Trash2 className="h-3.5 w-3.5" /></Button></>}
            {builtin && <a href={builtin.source} target="_blank" rel="noreferrer" className="ml-auto inline-flex items-center gap-1 rounded-full px-2 py-2 text-xs text-muted hover:text-primary focus-visible:outline-primary">{t("rewrites.documentation")}<ExternalLink className="h-3 w-3" /></a>}
          </div>
        </div>
        <details className="group border-t border-border"><summary className="flex cursor-pointer list-none items-center justify-between bg-fill/40 px-5 py-3 text-xs text-secondary hover:bg-fill [&::-webkit-details-marker]:hidden">{t("rewrites.viewRules")} · {SERVERS[item.server]}<ChevronDown className="h-3.5 w-3.5 transition-transform group-open:rotate-180" /></summary><pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words p-5 font-mono text-xs leading-relaxed text-secondary">{item.content}</pre></details>
      </article>)}
    </div>
    <Dialog open={!!draft} onOpenChange={open => { if (!open) closeDraft(); }}><DialogContent className="max-h-[90dvh] max-w-4xl overflow-y-auto"><DialogHeader><DialogTitle>{editing == null ? t("rewrites.newTitle") : t("rewrites.editTitle")}</DialogTitle><DialogDescription>{t("rewrites.dialogDescription")}</DialogDescription></DialogHeader>{draft && <>
      <div className="grid gap-4 sm:grid-cols-[1fr_180px]"><div className="space-y-2"><Label htmlFor="rewrite-name">{t("rewrites.name")}</Label><Input id="rewrite-name" placeholder={t("rewrites.namePlaceholder")} maxLength={80} value={draft.name} onChange={event => setDraft({ ...draft, name: event.target.value })} disabled={busy} /></div><div className="space-y-2"><Label htmlFor="rewrite-server">{t("rewrites.server")}</Label><Select value={draft.server} disabled={busy} onValueChange={(value: CustomRewrite["server"]) => setDraft({ ...draft, server: value })}><SelectTrigger id="rewrite-server"><SelectValue /></SelectTrigger><SelectContent>{Object.entries(SERVERS).map(([key, name]) => <SelectItem key={key} value={key}>{name}</SelectItem>)}</SelectContent></Select></div></div>
      <p className="text-xs leading-relaxed text-muted">{t(draft.server === "caddy" ? "rewrites.caddyHint" : draft.server === "nginx" ? "rewrites.nginxHint" : "rewrites.apacheHint")} {t("rewrites.switchServerHint")}</p>
      <CodeEditor label={t("rewrites.ruleLabel")} language={draft.server} value={draft.content} onChange={content => setDraft({ ...draft, content })} readOnly={busy} height="min(45dvh, 420px)" />
      <div className="flex justify-end gap-2"><Button variant="ghost" disabled={busy} onClick={closeDraft}>{t("common.cancel")}</Button><Button disabled={busy || !settings.isSuccess || !draft.name.trim() || !draft.content.trim()} onClick={save}>{busy ? t("common.saving") : t("rewrites.save")}</Button></div>
    </>}</DialogContent></Dialog>
    <ConfirmDialog open={discard} onOpenChange={setDiscard} title={t("rewrites.discardTitle")} description={t("rewrites.discardDescription")} confirmText={t("rewrites.discardConfirm")} onConfirm={() => { setDiscard(false); setDraft(null); }} />
    <ConfirmDialog open={remove != null} onOpenChange={open => { if (!open && !busy) setRemove(null); }} title={t("rewrites.deleteTitle")} description={t("rewrites.deleteDescription")} confirmText={t("common.delete")} danger loading={busy} onConfirm={() => void persist(templates.filter((_, index) => index !== remove))} />
  </div>;
}
