"use client";

import * as React from "react";
import type { SiteAccess } from "@nsb/schema";
import { Plus, Trash2 } from "lucide-react";
import { useT } from "@/lib/store";
import { siteAccessProblem, validSiteAccessAddress } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export function SiteAccessSettings({ value, disabled, onChange }: {
  value?: SiteAccess; disabled: boolean; onChange: (value?: SiteAccess) => void;
}) {
  const t = useT();
  const drafts = React.useRef<Record<SiteAccess["mode"], string[]>>({ allow: ["127.0.0.1", "::1"], deny: [""] });
  if (value) drafts.current[value.mode] = value.addresses;
  const problem = siteAccessProblem(value);
  return <div className="min-w-0 space-y-5">
    <p className="text-xs leading-relaxed text-secondary">{t("siteAccess.description")}</p>
    <div className="space-y-2">
      <Label htmlFor="site-access-mode">{t("siteAccess.mode")}</Label>
      <Select value={value?.mode ?? "off"} disabled={disabled} onValueChange={(mode) => {
        onChange(mode === "off" ? undefined : { mode: mode as SiteAccess["mode"], addresses: [...drafts.current[mode as SiteAccess["mode"]]] });
      }}>
        <SelectTrigger id="site-access-mode" className="w-full min-w-0"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="off">{t("siteAccess.off")}</SelectItem>
          <SelectItem value="allow">{t("siteAccess.allow")}</SelectItem>
          <SelectItem value="deny">{t("siteAccess.deny")}</SelectItem>
        </SelectContent>
      </Select>
      <p className="text-xs leading-relaxed text-muted">{t(value?.mode === "allow" ? "siteAccess.allowHint" : value?.mode === "deny" ? "siteAccess.denyHint" : "siteAccess.offHint")}</p>
    </div>
    {value && <>
      <div className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p className="text-[13px] font-medium text-secondary">{t("siteAccess.addresses")} · {value.addresses.length}/32</p>
          {value.mode === "allow" && <Button size="sm" variant="secondary" disabled={disabled} onClick={() => onChange({ mode: "allow", addresses: ["127.0.0.1", "::1"] })}>{t("siteAccess.localOnly")}</Button>}
        </div>
        <p className="text-xs leading-relaxed text-muted">{t("siteAccess.addressHint")}</p>
        {value.addresses.map((address, index) => {
          const valid = validSiteAccessAddress(address);
          return <div key={index} className="space-y-1.5">
            <Label htmlFor={`site-access-address-${index}`} className="sr-only">{t("siteAccess.address")} {index + 1}</Label>
            <div className="flex items-center gap-2">
              <Input id={`site-access-address-${index}`} value={address} disabled={disabled} placeholder="192.168.1.20"
                spellCheck={false} autoCapitalize="none" className="min-w-0 font-mono text-xs"
                aria-invalid={!valid} aria-describedby={!valid ? "site-access-problem" : undefined}
                onChange={(event) => onChange({ ...value, addresses: value.addresses.map((item, i) => i === index ? event.target.value : item) })} />
              <Button variant="ghost" size="icon-sm" disabled={disabled || value.addresses.length === 1} aria-label={`${t("siteAccess.remove")} ${index + 1}`}
                onClick={() => { onChange({ ...value, addresses: value.addresses.filter((_, i) => i !== index) }); requestAnimationFrame(() => document.getElementById(`site-access-address-${Math.max(0, index - 1)}`)?.focus()); }}><Trash2 className="h-3.5 w-3.5" /></Button>
            </div>
          </div>;
        })}
        <Button size="sm" variant="secondary" disabled={disabled || value.addresses.length >= 32} onClick={() => {
          const index = value.addresses.length; onChange({ ...value, addresses: [...value.addresses, ""] });
          requestAnimationFrame(() => document.getElementById(`site-access-address-${index}`)?.focus());
        }}><Plus className="h-3.5 w-3.5" />{t("siteAccess.add")}</Button>
        {problem && <p id="site-access-problem" role="alert" className="text-xs leading-relaxed text-error">{t(problem)}</p>}
      </div>
      <div className="space-y-2 border-t border-dashed border-separator pt-4 text-xs leading-relaxed text-muted">
        <p>{t("siteAccess.localHint")}</p>
        <p>{t("siteAccess.proxyHint")}</p>
      </div>
    </>}
    <p className="text-xs leading-relaxed text-muted">{t("siteAccess.saveHint")}</p>
  </div>;
}
