"use client";

import * as React from "react";
import type { SiteBasicAuth } from "@nsb/schema";
import { useT } from "@/lib/store";
import { siteBasicAuthProblem } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";

export function SiteBasicAuthSettings({ value, disabled, onChange }: {
  value?: SiteBasicAuth;
  disabled: boolean;
  onChange: (value?: SiteBasicAuth) => void;
}) {
  const t = useT();
  const [password, setPassword] = React.useState(value?.password ?? "");
  React.useEffect(() => setPassword(value?.password ?? ""), [value?.hasPassword]);
  const enabled = !!value?.enabled;
  const problem = siteBasicAuthProblem(value);
  const emit = (next: SiteBasicAuth) => onChange({ ...next, password: password || undefined });

  return <section className="space-y-4">
    <div className="space-y-1">
      <h3 className="text-sm font-medium">{t("basicAuth.title" as never)}</h3>
      <p className="text-xs leading-relaxed text-muted">{t("basicAuth.description" as never)}</p>
    </div>
    <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
      <div className="space-y-0.5">
        <Label htmlFor="site-basic-auth-enabled" className="text-[13px] font-medium">{t("basicAuth.enable" as never)}</Label>
        <p className="text-[11px] leading-relaxed text-muted">{t(enabled ? "basicAuth.enabledHint" as never : "basicAuth.disabledHint" as never)}</p>
      </div>
      <Switch id="site-basic-auth-enabled" checked={enabled} disabled={disabled} onCheckedChange={(checked) => {
        if (checked) emit(value ?? { enabled: true, username: "admin", hasPassword: false });
        else { setPassword(""); onChange(undefined); }
      }} />
    </div>
    {enabled && <div className="space-y-4 rounded-xl border border-border bg-fill/40 p-3.5">
      <div className="space-y-1.5">
        <Label htmlFor="site-basic-auth-username">{t("basicAuth.username" as never)}</Label>
        <Input id="site-basic-auth-username" value={value?.username ?? ""} disabled={disabled}
          onChange={(event) => emit({ ...value!, username: event.target.value })}
          autoCapitalize="none" spellCheck={false} aria-invalid={problem === "basicAuth.usernameInvalid"} />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="site-basic-auth-password">{t(value?.hasPassword ? "basicAuth.newPassword" as never : "basicAuth.password" as never)}</Label>
        <Input id="site-basic-auth-password" type="password" value={password} disabled={disabled}
          onChange={(event) => { setPassword(event.target.value); emit({ ...value!, password: event.target.value }); }}
          autoComplete="new-password" spellCheck={false} aria-invalid={problem === "basicAuth.passwordInvalid" || problem === "basicAuth.passwordRequired"} />
        <p className="text-[11px] leading-relaxed text-muted">{t("basicAuth.passwordHint" as never)}</p>
      </div>
      {problem && <p role="alert" className="text-xs leading-relaxed text-error">{t(problem as never)}</p>}
    </div>}
    <p className="text-xs leading-relaxed text-muted">{t("basicAuth.saveHint" as never)}</p>
  </section>;
}
