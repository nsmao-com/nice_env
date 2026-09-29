"use client";

import type { SiteRedirect } from "@nsb/schema";
import { useT } from "@/lib/store";
import { siteRedirectTarget } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export const DEFAULT_REDIRECT: SiteRedirect = { target: "", status: 302, preservePath: true };

export function SiteRedirectFields({ id, value, domains, disabled, onChange }: {
  id: string; value: SiteRedirect; domains: string[]; disabled?: boolean; onChange: (value: SiteRedirect) => void;
}) {
  const t = useT();
  const result = siteRedirectTarget(value, domains);
  const invalid = !!value.target.trim() && !!result.error;
  const example = result.url ? value.preservePath ? `${result.url.replace(/\/+$/, "")}/docs/start?lang=zh` : result.url : null;
  return <div className="flex min-w-0 flex-col gap-4">
    <p className="text-xs leading-relaxed text-muted">{t("redirect.hint")}</p>
    <div className="space-y-1.5">
      <Label htmlFor={`${id}-target`}>{t("redirect.target")}</Label>
      <Input id={`${id}-target`} value={value.target} disabled={disabled} placeholder="https://new.example.com"
        onChange={(event) => onChange({ ...value, target: event.target.value })} autoCapitalize="none" spellCheck={false}
        aria-invalid={invalid} aria-describedby={`${id}-result`} className="font-mono text-[12px]" />
      <p id={`${id}-result`} aria-live="polite" className={`text-xs leading-relaxed ${invalid ? "text-error" : "text-muted"}`}>
        {t(invalid ? result.error! : "redirect.targetHint")}
      </p>
    </div>
    <div className="space-y-1.5">
      <Label htmlFor={`${id}-status`}>{t("redirect.status")}</Label>
      <Select value={String(value.status)} disabled={disabled} onValueChange={(status) => onChange({ ...value, status: Number(status) as SiteRedirect["status"] })}>
        <SelectTrigger id={`${id}-status`}><SelectValue>{t(`redirect.code${value.status}`)}</SelectValue></SelectTrigger>
        <SelectContent>{([302, 301, 307, 308] as const).map((status) => <SelectItem key={status} value={String(status)}>{t(`redirect.code${status}`)}</SelectItem>)}</SelectContent>
      </Select>
      {[301, 308].includes(value.status) && <p className="text-xs leading-relaxed text-warn">{t("redirect.permanentHint")}</p>}
    </div>
    <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
      <div className="min-w-0 space-y-1"><Label htmlFor={`${id}-path`}>{t("redirect.preserve")}</Label><p className="text-xs leading-relaxed text-muted">{t("redirect.preserveHint")}</p></div>
      <Switch id={`${id}-path`} checked={value.preservePath} disabled={disabled} onCheckedChange={(preservePath) => onChange({ ...value, preservePath })} />
    </div>
    {example && <div className="space-y-2 rounded-lg bg-fill p-3 text-xs" aria-live="polite">
      <p className="font-medium">{t("redirect.preview")}</p>
      <code className="block text-muted [overflow-wrap:anywhere]">/docs/start?lang=zh</code>
      <code className="block text-secondary [overflow-wrap:anywhere]">→ {example}</code>
    </div>}
  </div>;
}
