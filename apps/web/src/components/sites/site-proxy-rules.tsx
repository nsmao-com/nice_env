"use client";

import type { SiteProxyRule } from "@nsb/schema";
import { Plus, Trash2 } from "lucide-react";
import { useT } from "@/lib/store";
import { useSites } from "@/lib/hooks";
import { normalizeProxyTarget, proxyRulePath, proxyRuleExample } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export function SiteProxyRules({ siteId, value = [], disabled, onChange }: {
  siteId: string; value?: SiteProxyRule[]; disabled: boolean; onChange: (rules: SiteProxyRule[]) => void;
}) {
  const t = useT();
  const { data: sites } = useSites();
  const targets = sites.filter((site) => site.id !== siteId && site.accessUrl).map((site) => ({ name: site.name, url: site.accessUrl! }));
  const choices = targets.filter((item, index) => targets.findIndex((candidate) => candidate.url === item.url) === index);
  const update = (index: number, patch: Partial<SiteProxyRule>) => onChange(value.map((rule, i) => i === index ? { ...rule, ...patch } : rule));
  return <div className="min-w-0 space-y-4">
    <p className="text-xs leading-relaxed text-secondary">{t("proxyRules.description")}</p>
    <p className="text-xs leading-relaxed text-muted">{t("proxyRules.priority")}</p>
    {!value.length && <div className="rounded-xl bg-fill px-4 py-5 text-center">
      <p className="text-[13px] font-medium">{t("proxyRules.empty")}</p>
      <p className="mt-2 text-xs leading-relaxed text-muted">{t("proxyRules.emptyHint")}</p>
    </div>}
    {value.map((rule, index) => {
      const path = proxyRulePath(rule.path), target = new TextEncoder().encode(rule.target).length <= 8192 ? normalizeProxyTarget(rule.target) : null, example = proxyRuleExample(rule);
      const duplicate = !!path && value.some((other, i) => i !== index && proxyRulePath(other.path) === path);
      return <section key={index} className="min-w-0 space-y-3.5 rounded-xl border border-border p-3.5" aria-label={t("proxyRules.rule").replace("{number}", String(index + 1))}>
        <div className="flex items-center justify-between gap-3">
          <h3 className="text-[13px] font-medium">{t("proxyRules.rule").replace("{number}", String(index + 1))}</h3>
          <Button variant="ghost" size="icon-sm" disabled={disabled} aria-label={t("proxyRules.remove").replace("{number}", String(index + 1))} onClick={() => {
            onChange(value.filter((_, i) => i !== index));
            requestAnimationFrame(() => document.getElementById(value.length > 1 ? `site-proxy-path-${Math.max(0, index - 1)}` : "site-proxy-add")?.focus());
          }}><Trash2 className="size-3.5" /></Button>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`site-proxy-path-${index}`}>{t("proxyRules.path")}</Label>
          <Input id={`site-proxy-path-${index}`} value={rule.path} disabled={disabled} placeholder="/api" spellCheck={false} autoCapitalize="none"
            className="min-w-0 font-mono text-xs" aria-invalid={!path || duplicate} aria-describedby={`site-proxy-path-hint-${index}`}
            onChange={(event) => update(index, { path: event.target.value })} />
          <p id={`site-proxy-path-hint-${index}`} role={!path || duplicate ? "alert" : undefined} className={`text-xs leading-relaxed ${!path || duplicate ? "text-error" : "text-muted"}`}>{t(!path ? "proxyRules.pathInvalid" : duplicate ? "proxyRules.duplicate" : "proxyRules.pathHint")}</p>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`site-proxy-target-${index}`}>{t("proxyRules.target")}</Label>
          <Input id={`site-proxy-target-${index}`} value={rule.target} disabled={disabled} placeholder="http://127.0.0.1:8081" spellCheck={false} autoCapitalize="none"
            className="min-w-0 font-mono text-xs" aria-invalid={!target} aria-describedby={`site-proxy-target-hint-${index}`}
            onChange={(event) => update(index, { target: event.target.value })} />
          {!!choices.length && <Select value="" disabled={disabled} onValueChange={(target) => update(index, { target })}>
            <SelectTrigger className="w-full min-w-0" aria-label={t("proxyRules.chooseTarget")}><SelectValue placeholder={t("proxyRules.chooseTarget")} /></SelectTrigger>
            <SelectContent>{choices.map((item) => <SelectItem key={item.url} value={item.url}>{item.name} · {item.url}</SelectItem>)}</SelectContent>
          </Select>}
          <p id={`site-proxy-target-hint-${index}`} role={!target ? "alert" : undefined} className={`text-xs leading-relaxed ${target ? "text-muted" : "text-error"}`}>{t(target ? "proxyRules.targetHint" : "proxyRules.targetInvalid")}</p>
        </div>
        <div className="flex items-center justify-between gap-4 rounded-lg bg-fill p-3">
          <div className="min-w-0 space-y-1"><Label htmlFor={`site-proxy-strip-${index}`}>{t("proxyRules.strip")}</Label><p className="text-xs leading-relaxed text-muted">{t("proxyRules.stripHint")}</p></div>
          <Switch id={`site-proxy-strip-${index}`} checked={rule.stripPrefix} disabled={disabled} onCheckedChange={(stripPrefix) => update(index, { stripPrefix })} />
        </div>
        {example && <div className="space-y-1 rounded-lg bg-fill/60 p-3 text-xs leading-relaxed" aria-live="polite">
          <p className="text-muted">{t("proxyRules.preview")}</p>
          <p className="font-mono [overflow-wrap:anywhere]">{path}/users?limit=10</p>
          <p className="font-mono text-secondary [overflow-wrap:anywhere]">→ {example}</p>
        </div>}
      </section>;
    })}
    <Button id="site-proxy-add" variant="secondary" size="sm" disabled={disabled || value.length >= 16} onClick={() => {
      const index = value.length; onChange([...value, { path: value.length ? "" : "/api", target: "", stripPrefix: true }]);
      requestAnimationFrame(() => document.getElementById(`site-proxy-${value.length ? "path" : "target"}-${index}`)?.focus());
    }}><Plus className="size-3.5" />{t("proxyRules.add")}</Button>
    <p className="text-xs leading-relaxed text-muted">{t("proxyRules.applyHint")}</p>
  </div>;
}
