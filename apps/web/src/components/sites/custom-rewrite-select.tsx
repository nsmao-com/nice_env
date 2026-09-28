"use client";
import type { CustomRewrite } from "@nsb/schema";
import { useSettings } from "@/lib/hooks";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CodeEditor } from "@/components/shared/code-editor";
import Link from "next/link";

export function CustomRewriteSelect({ server, value, onChange, disabled }: { server: string; value?: CustomRewrite; onChange: (value?: CustomRewrite) => void; disabled?: boolean }) {
  const settings = useSettings();
  const templates = (settings.data?.rewriteTemplates ?? []).filter((item) => item.server === server);
  return <div className="space-y-2 rounded-xl border border-border p-3">
    <div className="flex items-center justify-between gap-3 text-xs"><span>自定义伪静态模板</span><Link className="text-primary hover:underline" href="/rewrites">管理模板</Link></div>
    <Select value={value ? "snapshot" : "builtin"} disabled={disabled} onValueChange={(id) => onChange(id === "builtin" ? undefined : templates[Number(id)])}>
      <SelectTrigger aria-label="自定义伪静态模板"><SelectValue /></SelectTrigger>
      <SelectContent><SelectItem value="builtin">使用上方内置规则</SelectItem>{value && <SelectItem value="snapshot">{value.name}（站点副本）</SelectItem>}{templates.map((item, index) => <SelectItem key={`${item.server}:${item.name}`} value={String(index)}>{item.name}</SelectItem>)}</SelectContent>
    </Select>
    {value && <><p className="text-xs text-muted">规则副本随站点保存；模板之后的修改或删除不会影响此站点。保存站点后生效。</p><CodeEditor label="站点伪静态规则" language={server === "nginx" ? "nginx" : "apache"} value={value.content} onChange={(content) => onChange({ ...value, content })} readOnly={disabled} height="200px" /></>}
  </div>;
}
