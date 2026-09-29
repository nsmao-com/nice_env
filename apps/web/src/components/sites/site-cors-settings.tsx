"use client";

import * as React from "react";
import type { SiteCors } from "@nsb/schema";
import { Plus, Trash2 } from "lucide-react";
import { useT } from "@/lib/store";
import { useSites } from "@/lib/hooks";
import { CORS_METHODS, corsOrigin, siteCorsProblem } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

const initialCors = (): SiteCors => ({ origins: [""], methods: ["GET", "HEAD", "POST"], allowedHeaders: ["Content-Type", "Authorization"], exposedHeaders: [], credentials: false, maxAge: 600 });

export function SiteCorsSettings({ value, disabled, onChange }: {
  value?: SiteCors; disabled: boolean; onChange: (value?: SiteCors) => void;
}) {
  const t = useT();
  const { data: sites } = useSites();
  const previous = React.useRef<SiteCors | undefined>(value);
  const specificOrigins = React.useRef<string[]>(value?.origins.filter((origin) => origin.trim() !== "*") ?? [""]);
  const allowedOrigins = [...new Set(sites.map((site) => {
    try { return site.accessUrl ? new URL(site.accessUrl).origin : ""; } catch { return ""; }
  }).filter(Boolean).concat(["http://localhost:3000", "http://127.0.0.1:3000"]))];
  const problem = siteCorsProblem(value);
  const anyOrigin = value?.origins.some((origin) => origin.trim() === "*") ?? false;
  const change = (next: SiteCors) => { previous.current = next; onChange(next); };
  return <div className="min-w-0 space-y-5">
    <div className="space-y-2 text-xs leading-relaxed text-secondary">
      <p>{t("cors.description")}</p>
      <p>{t("cors.scopeHint")}</p>
    </div>
    <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
      <div className="min-w-0 space-y-1"><Label htmlFor="site-cors-enabled">{t("cors.enable")}</Label><p className="text-xs leading-relaxed text-muted">{t("cors.offHint")}</p></div>
      <Switch id="site-cors-enabled" checked={!!value} disabled={disabled} onCheckedChange={(enabled) => {
        if (enabled) change(previous.current ?? initialCors()); else { previous.current = value; onChange(undefined); }
      }} />
    </div>
    {value && <>
      <div className="space-y-3">
        <div className="flex items-center justify-between gap-4">
          <Label htmlFor="site-cors-any">{t("cors.anyOrigin")}</Label>
          <Switch id="site-cors-any" checked={anyOrigin} disabled={disabled} onCheckedChange={(any) => {
            if (any) { specificOrigins.current = value.origins; change({ ...value, origins: ["*"], credentials: false }); }
            else change({ ...value, origins: specificOrigins.current.length ? specificOrigins.current : [""] });
          }} />
        </div>
        {anyOrigin ? <p className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t("cors.anyHint")}</p>
          : <div className="space-y-2">
            <p className="text-xs text-muted">{t("cors.originHint")}</p>
            {value.origins.map((origin, index) => <div key={index} className="space-y-1.5">
              <Label htmlFor={`site-cors-origin-${index}`} className="sr-only">{t("cors.origin")} {index + 1}</Label>
              <div className="flex items-center gap-2">
                <Input id={`site-cors-origin-${index}`} value={origin} disabled={disabled} placeholder="https://frontend.test:8443" spellCheck={false} autoCapitalize="none"
                  aria-invalid={!corsOrigin(origin)} aria-describedby={!corsOrigin(origin) ? "site-cors-problem" : undefined}
                  onChange={(event) => change({ ...value, origins: value.origins.map((item, i) => i === index ? event.target.value : item) })} className="min-w-0 font-mono text-xs" />
                <Button variant="ghost" size="icon-sm" disabled={disabled || value.origins.length === 1} aria-label={`${t("cors.removeOrigin")} ${index + 1}`}
                  onClick={() => { change({ ...value, origins: value.origins.filter((_, i) => i !== index) }); requestAnimationFrame(() => document.getElementById(`site-cors-origin-${Math.max(0, index - 1)}`)?.focus()); }}><Trash2 className="h-3.5 w-3.5" /></Button>
              </div>
            </div>)}
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="secondary" size="sm" disabled={disabled || value.origins.length >= 32} onClick={() => {
                const index = value.origins.length; change({ ...value, origins: [...value.origins, ""] }); requestAnimationFrame(() => document.getElementById(`site-cors-origin-${index}`)?.focus());
              }}><Plus className="h-3.5 w-3.5" />{t("cors.addOrigin")}</Button>
              <Select value="" disabled={disabled || value.origins.length >= 32} onValueChange={(origin) => {
                const existing = value.origins.filter((item) => item.trim()); change({ ...value, origins: [...new Set([...existing, origin])] });
              }}>
                <SelectTrigger className="min-w-0 w-full sm:w-auto sm:max-w-[260px]" aria-label={t("cors.chooseOrigin")}><SelectValue placeholder={t("cors.chooseOrigin")} /></SelectTrigger>
                <SelectContent>{allowedOrigins.filter((origin) => !value.origins.includes(origin)).map((origin) => <SelectItem key={origin} value={origin}>{origin}</SelectItem>)}</SelectContent>
              </Select>
            </div>
          </div>}
      </div>
      <fieldset className="space-y-2" disabled={disabled}>
        <legend className="mb-2 text-[13px] font-medium text-secondary">{t("cors.methods")}</legend>
        <div className="flex flex-wrap gap-2">
          {CORS_METHODS.map((method) => <label key={method} className="flex cursor-pointer items-center gap-2 rounded-lg bg-fill px-3 py-2 text-xs">
            <input type="checkbox" className="accent-primary" checked={value.methods.includes(method)} onChange={(event) => change({ ...value, methods: event.target.checked ? [...value.methods, method] : value.methods.filter((item) => item !== method) })} />
            {method}
          </label>)}
        </div>
      </fieldset>
      <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
        <div className="min-w-0 space-y-1"><Label htmlFor="site-cors-credentials">{t("cors.credentials")}</Label><p className="text-xs leading-relaxed text-muted">{t("cors.credentialsHint")}</p></div>
        <Switch id="site-cors-credentials" checked={value.credentials} disabled={disabled || anyOrigin} onCheckedChange={(credentials) => change({ ...value, credentials })} />
      </div>
      <details className="rounded-xl border border-border p-3.5" open={problem === "cors.headersInvalid" || problem === "cors.ageInvalid" ? true : undefined}>
        <summary className="cursor-pointer text-[13px] font-medium">{t("cors.advanced")}</summary>
        <div className="mt-4 space-y-5">
          <CorsHeaders id="allowed" label={t("cors.allowedHeaders")} hint={t("cors.allowedHint")} values={value.allowedHeaders} disabled={disabled}
            suggestions={["Content-Type", "Authorization", "X-Requested-With", "X-CSRF-Token"]} onChange={(allowedHeaders) => change({ ...value, allowedHeaders })} />
          <CorsHeaders id="exposed" label={t("cors.exposedHeaders")} hint={t("cors.exposedHint")} values={value.exposedHeaders} disabled={disabled}
            suggestions={["Content-Disposition", "X-Total-Count", "X-Request-Id", "ETag"]} onChange={(exposedHeaders) => change({ ...value, exposedHeaders })} />
          <div className="space-y-1.5">
            <Label htmlFor="site-cors-age">{t("cors.maxAge")}</Label>
            <Input id="site-cors-age" type="number" min={0} max={86400} value={Number.isFinite(value.maxAge) ? value.maxAge : ""} disabled={disabled}
              aria-invalid={!Number.isInteger(value.maxAge) || value.maxAge < 0 || value.maxAge > 86400}
              onChange={(event) => change({ ...value, maxAge: event.target.value === "" ? NaN : Number(event.target.value) })} />
            <p className="text-xs leading-relaxed text-muted">{t("cors.ageHint")}</p>
          </div>
        </div>
      </details>
      {problem && <p id="site-cors-problem" role="alert" className="text-xs leading-relaxed text-error">{t(problem)}</p>}
    </>}
  </div>;
}

function CorsHeaders({ id, label, hint, values, suggestions, disabled, onChange }: {
  id: string; label: string; hint: string; values: string[]; suggestions: string[]; disabled: boolean; onChange: (values: string[]) => void;
}) {
  const t = useT();
  return <div className="space-y-2">
    <p className="text-[13px] font-medium text-secondary">{label}</p>
    <p className="text-xs leading-relaxed text-muted">{hint}</p>
    <div className="flex flex-wrap gap-2">{suggestions.map((name) => {
      const selected = values.some((value) => value.toLowerCase() === name.toLowerCase());
      return <Button key={name} variant={selected ? "secondary" : "ghost"} size="sm" disabled={disabled || (!selected && values.length >= 64)} aria-pressed={selected}
        onClick={() => onChange(selected ? values.filter((value) => value.toLowerCase() !== name.toLowerCase()) : [...values, name])}>{name}</Button>;
    })}</div>
    {values.map((name, index) => <div key={index} className="flex items-center gap-2">
      <Label className="sr-only" htmlFor={`site-cors-${id}-${index}`}>{label} {index + 1}</Label>
      <Input id={`site-cors-${id}-${index}`} value={name} disabled={disabled} className="min-w-0 font-mono text-xs" spellCheck={false}
        aria-invalid={!/^[A-Za-z0-9_-]{1,128}$/.test(name.trim())}
        onChange={(event) => onChange(values.map((value, i) => i === index ? event.target.value : value))} />
      <Button variant="ghost" size="icon-sm" disabled={disabled} aria-label={`${t("cors.removeHeader")} ${index + 1}`} onClick={() => {
        onChange(values.filter((_, i) => i !== index));
        requestAnimationFrame(() => document.getElementById(values.length > 1 ? `site-cors-${id}-${Math.max(0, index - 1)}` : `site-cors-${id}-add`)?.focus());
      }}><Trash2 className="h-3.5 w-3.5" /></Button>
    </div>)}
    <Button id={`site-cors-${id}-add`} variant="secondary" size="sm" disabled={disabled || values.length >= 64} onClick={() => {
      const index = values.length; onChange([...values, ""]);
      requestAnimationFrame(() => document.getElementById(`site-cors-${id}-${index}`)?.focus());
    }}><Plus className="h-3.5 w-3.5" />{t("cors.addHeader")}</Button>
  </div>;
}
