"use client";

import * as React from "react";
import type { CustomRewrite } from "@nsb/schema";
import { useSettings } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { CodeEditor } from "@/components/shared/code-editor";
import { REWRITE_TEMPLATES } from "@/lib/rewrite-templates";
import { BookOpen, Check, ChevronsUpDown } from "lucide-react";
import Link from "next/link";

export function CustomRewriteSelect({ server, value, onChange, disabled }: { server: string; value?: CustomRewrite; onChange: (value?: CustomRewrite) => void; disabled?: boolean }) {
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
    <div className="flex flex-wrap items-center justify-between gap-2 text-xs"><span className="inline-flex items-center gap-2 font-medium"><BookOpen className="h-4 w-4 text-primary" />从模板库选择<span className="font-normal text-muted">{builtins.length + templates.length} 个可用</span></span><Link className="rounded px-1 py-1 text-primary hover:underline" href="/rewrites">管理模板</Link></div>
    <Popover open={open && !disabled} onOpenChange={setOpen}>
      <PopoverTrigger asChild><Button type="button" variant="secondary" role="combobox" aria-expanded={open} aria-label="选择伪静态模板" disabled={disabled} className="h-auto min-h-10 w-full justify-between rounded-lg border border-border px-3 text-left font-normal"><span className="min-w-0 truncate">{value?.name ?? "使用上方基础规则，或搜索更多模板…"}</span><ChevronsUpDown className="h-4 w-4 shrink-0 text-muted" /></Button></PopoverTrigger>
      <PopoverContent align="start" className="w-[var(--radix-popover-trigger-width)] min-w-[min(320px,calc(100vw-32px))] max-w-[calc(100vw-32px)] p-0">
        <Command filter={(value, search) => value.toLowerCase().includes(search.trim().toLowerCase()) ? 1 : 0}><CommandInput aria-label="搜索伪静态模板" placeholder="搜索框架、应用或用途…" /><CommandList>
          <CommandEmpty>未找到模板，试试其他关键词。</CommandEmpty>
          <CommandGroup heading="基础规则"><CommandItem value="使用上方基础规则" onSelect={() => choose()}>使用上方基础规则{!value && <Check className="ml-auto h-4 w-4" />}</CommandItem></CommandGroup>
          {value && <CommandGroup heading="当前站点"><CommandItem value={`snapshot ${value.name}`} onSelect={() => setOpen(false)}><span className="truncate">{value.name}（站点副本）</span><Check className="ml-auto h-4 w-4 shrink-0" /></CommandItem></CommandGroup>}
          {(["PHP 框架", "内容管理", "静态站点"] as const).map(category => <CommandGroup key={category} heading={category}>{builtins.filter(item => item.category === category).map(item => <CommandItem key={item.name} value={`builtin ${item.name} ${item.description}`} onSelect={() => choose(item)} className="items-start py-2.5"><div className="min-w-0"><div className="text-sm font-medium">{item.name}</div><div className="mt-1 whitespace-normal text-xs leading-relaxed text-muted">{item.description}</div></div></CommandItem>)}</CommandGroup>)}
          {!!templates.length && <CommandGroup heading="我的模板">{templates.map((item, index) => <CommandItem key={index} value={`custom ${index} ${item.name}`} onSelect={() => choose(item)}>{item.name}</CommandItem>)}</CommandGroup>}
        </CommandList></Command>
        {settings.isError && <p className="border-t border-border px-3 py-2 text-xs text-error">自定义模板读取失败 <button type="button" className="underline" onClick={() => void settings.refetch()}>重试</button></p>}
      </PopoverContent>
    </Popover>
    {value && <>
      {selected && <p className="text-xs leading-relaxed text-secondary">运行目录：<strong className="font-medium">{selected.documentRoot}</strong> · {selected.description}</p>}
      <p className="text-xs leading-relaxed text-muted">下方规则随站点保存；模板之后的修改或删除不会影响此站点。保存站点后生效。</p>
      <CodeEditor label="站点伪静态规则" language={server} value={value.content} onChange={content => onChange({ ...value, content })} readOnly={disabled} height="280px" />
    </>}
  </div>;
}
