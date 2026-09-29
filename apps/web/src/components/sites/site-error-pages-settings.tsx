"use client";

import * as React from "react";
import type { SiteRuntime } from "@nsb/schema";
import { useT } from "@/lib/store";
import { SITE_ERROR_STATUSES, validSiteErrorPagePath } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";

type ErrorPages = NonNullable<SiteRuntime["errorPages"]>;

export function SiteErrorPagesSettings({
  value,
  disabled,
  onChange,
}: {
  value: SiteRuntime["errorPages"];
  disabled?: boolean;
  onChange: (value: SiteRuntime["errorPages"]) => void;
}) {
  const t = useT();
  const pages = value ?? {};
  const change = (next: ErrorPages) => onChange(Object.keys(next).length ? next : undefined);

  return (
    <section className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-sm font-medium">{t("errorPages.title" as never)}</h3>
        <p className="text-xs leading-relaxed text-muted">{t("errorPages.description" as never)}</p>
      </div>
      <div className="rounded-xl border border-border bg-fill/40 px-3.5">
        {SITE_ERROR_STATUSES.map((status, index) => {
          const key = String(status);
          const path = pages[key] ?? "";
          const enabled = Object.prototype.hasOwnProperty.call(pages, key);
          const invalid = enabled && !validSiteErrorPagePath(path);
          return (
            <div key={status} className={`grid gap-3 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(190px,1.25fr)] sm:items-center ${index ? "border-t border-dashed border-separator" : ""}`}>
              <div className="flex min-w-0 items-center justify-between gap-3">
                <div className="min-w-0 space-y-0.5">
                  <Label htmlFor={`site-error-page-${status}`} className="text-[13px] font-medium">
                    HTTP {status} · {t(`errorPages.status.${status}` as never)}
                  </Label>
                  <p className="text-[11px] leading-relaxed text-muted">{t("errorPages.statusHint" as never)}</p>
                </div>
                <Switch
                  aria-label={`${t("errorPages.enable" as never)} HTTP ${status}`}
                  checked={enabled}
                  disabled={disabled}
                  onCheckedChange={(checked) => {
                    if (checked) change({ ...pages, [key]: path || `/${status}.html` });
                    else {
                      const next = { ...pages };
                      delete next[key];
                      change(next);
                    }
                  }}
                />
              </div>
              <div className="min-w-0 space-y-1">
                <Label htmlFor={`site-error-page-${status}`} className="sr-only">{t("errorPages.path" as never)}</Label>
                <Input
                  id={`site-error-page-${status}`}
                  value={path}
                  disabled={disabled || !enabled}
                  onChange={(event) => change({ ...pages, [key]: event.target.value })}
                  placeholder={`/${status}.html`}
                  className="font-mono text-[12px]"
                  spellCheck={false}
                  autoCapitalize="none"
                  aria-invalid={invalid}
                />
                {invalid && <p className="text-[11px] leading-relaxed text-error">{t("errorPages.invalid" as never)}</p>}
              </div>
            </div>
          );
        })}
      </div>
      <p className="text-xs leading-relaxed text-muted">{t("errorPages.pathHint" as never)}</p>
    </section>
  );
}
