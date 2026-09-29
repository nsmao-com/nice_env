"use client";

import * as React from "react";
import type { CustomRewrite } from "@nsb/schema";
import { useSettings } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { CodeEditor } from "@/components/shared/code-editor";
import { REWRITE_TEMPLATES } from "@/lib/rewrite-templates";
import { BookOpen, Check, ChevronsUpDown } from "lucide-react";
import Link from "next/link";

const CATEGORIES = [
  { value: "PHP 框架", labelKey: "rewrites.phpFramework" },
  { value: "内容管理", labelKey: "rewrites.cms" },
  { value: "静态站点", labelKey: "rewrites.staticSites" },
] as const;

export function CustomRewriteSelect({ server, value, onChange, disabled }: { server: string; value?: CustomRewrite; onChange: (value?: CustomRewrite) => void; disabled?: boolean }) {
  const t = useT();
  const settings = useSettings();
  const [open, setOpen] = React.useState(false);
  const builtins = REWRITE_TEMPLATES.filter(item => item.server === server);
  const templates = (settings.data?.rewriteTemplates ?? []).filter(item => item.server === server);
  const selected = builtins.find(item => item.name === value?.name && item.content === value?.content);
  const choose = (item?: CustomRewrite) => {
    onChange(item ? { name: item.name, server: item.server, content: item.content } : undefined);
    setOpen(false);
  };
  return <div className="space-y-3 rounded-xl border border-border bg-fill/30 p-4">
    <div className="flex flex-wrap items-center justify-between gap-2 text-xs"><span className="inline-flex items-center gap-2 font-medium"><BookOpen className="h-4 w-4 text-primary" />{t("rewrites.selectTitle")}<span className="font-normal text-muted">{t("rewrites.available").replace("{count}", String(builtins.length + templates.length))}</span></span><Link className="rounded px-1 py-1 text-primary hover:underline" href="/rewrites">{t("rewrites.manage")}</Link></div>
    <Popover open={open && !disabled} onOpenChange={setOpen}>
      <PopoverTrigger asChild><Button type="button" variant="secondary" role="combobox" aria-expanded={open} aria-label={t("rewrites.selectAria")} disabled={disabled} className="h-auto min-h-10 w-full justify-between rounded-lg border border-border px-3 text-left font-normal"><span className="min-w-0 truncate">{value?.name ?? t("rewrites.selectPlaceholder")}</span><ChevronsUpDown className="h-4 w-4 shrink-0 text-muted" /></Button></PopoverTrigger>
      <PopoverContent align="start" className="w-[var(--radix-popover-trigger-width)] min-w-[min(320px,calc(100vw-32px))] max-w-[calc(100vw-32px)] p-0">
        <Command filter={(value, search) => value.toLowerCase().includes(search.trim().toLowerCase()) ? 1 : 0}><CommandInput aria-label={t("rewrites.searchAria")} placeholder={t("rewrites.searchPlaceholder")} /><CommandList>
          <CommandEmpty>{t("rewrites.noMatchesHint")}</CommandEmpty>
          <CommandGroup heading={t("rewrites.baseRules")}><CommandItem value={t("rewrites.useBase")} onSelect={() => choose()}>{t("rewrites.useBase")}{!value && <Check className="ml-auto h-4 w-4" />}</CommandItem></CommandGroup>
          {value && <CommandGroup heading={t("rewrites.currentSite")}><CommandItem value={`snapshot ${value.name}`} onSelect={() => setOpen(false)}><span className="truncate">{value.name}（{t("rewrites.siteCopySuffix")}）</span><Check className="ml-auto h-4 w-4 shrink-0" /></CommandItem></CommandGroup>}
          {CATEGORIES.map(category => <CommandGroup key={category.value} heading={t(category.labelKey)}>{builtins.filter(item => item.category === category.value).map(item => <CommandItem key={item.name} value={`builtin ${item.name} ${item.description}`} onSelect={() => choose(item)} className="items-start py-2.5"><div className="min-w-0"><div className="text-sm font-medium">{item.name}</div><div className="mt-1 whitespace-normal text-xs leading-relaxed text-muted">{item.description}</div></div></CommandItem>)}</CommandGroup>)}
          {!!templates.length && <CommandGroup heading={t("rewrites.customTemplates")}>{templates.map((item, index) => <CommandItem key={index} value={`custom ${index} ${item.name}`} onSelect={() => choose(item)}>{item.name}</CommandItem>)}</CommandGroup>}
        </CommandList></Command>
        {settings.isError && <p className="mx-2 border-t border-dashed border-separator px-1 py-2 text-xs text-error">{t("rewrites.loadFailed")} <button type="button" className="underline" onClick={() => void settings.refetch()}>{t("common.retry")}</button></p>}
      </PopoverContent>
    </Popover>
    {value && <>
      {selected && <p className="text-xs leading-relaxed text-secondary">{t("rewrites.documentRoot")}：<strong className="font-medium">{selected.documentRoot}</strong> · {selected.description}</p>}
      <p className="text-xs leading-relaxed text-muted">{t("rewrites.siteHint")}</p>
      <CodeEditor label={t("rewrites.siteRuleLabel")} language={server} value={value.content} onChange={content => onChange({ ...value, content })} readOnly={disabled} height="280px" />
    </>}
  </div>;
}
