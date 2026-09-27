"use client";

import { Trash2 } from "lucide-react";
import { useT } from "@/lib/store";
import { PHP_SITE_OPTIONS, isPhpSiteSettingValid } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export function SitePhpSettings({ values, previousValues, rootDir, disabled, onChange }: {
  values: Record<string, string>;
  previousValues: Record<string, string>;
  rootDir: string;
  disabled: boolean;
  onChange: (values: Record<string, string>) => void;
}) {
  const t = useT();
  const entries = Object.entries(values);
  const available = PHP_SITE_OPTIONS.filter((option) => !(option.key in values));
  const setValue = (key: string, value: string) => onChange({ ...values, [key]: value });
  const remove = (key: string) => {
    const next = { ...values }; delete next[key]; onChange(next);
    requestAnimationFrame(() => document.getElementById("site-php-add")?.focus());
  };
  return (
    <div className="space-y-4">
      <div className="space-y-2 text-xs leading-relaxed text-secondary">
        <p>{t("sites.php.description")}</p>
        <code className="block text-foreground [overflow-wrap:anywhere]">{rootDir.replace(/[\\/]+$/, "")}/.user.ini</code>
        <p>{t("sites.php.cacheSummary")}</p>
        <details><summary className="cursor-pointer">{t("sites.php.applyDetails")}</summary><p className="mt-2">{t("sites.php.cacheHint")}</p></details>
      </div>
      {entries.length === 0 && <p className="rounded-xl bg-fill p-4 text-sm text-secondary">{t("sites.php.empty")}</p>}
      {entries.map(([key, value]) => {
        const option = PHP_SITE_OPTIONS.find((option) => option.key === key);
        const label = option ? t(`sites.php.${option.key}`) : key;
        const valid = isPhpSiteSettingValid(key, value, previousValues[key]);
        const id = `site-php-${key}`;
        const size = /^(\d*)([kmg]?)$/i.exec(value);
        const unlimited = value === "-1";
        const hint = option?.type === "size" ? "sites.php.sizeHint" : option?.type === "number"
          ? ["max_input_vars", "max_file_uploads"].includes(key) ? "sites.php.countHint" : "sites.php.numberHint"
          : "sites.php.booleanHint";
        return <div key={key} className="space-y-2 rounded-xl bg-fill p-3.5">
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0 space-y-1">
              <Label htmlFor={id} className="text-foreground">{label}</Label>
              <p className="font-mono text-[11px] text-secondary [overflow-wrap:anywhere]">{key}</p>
            </div>
            <Button variant="ghost" size="icon-sm" disabled={disabled} aria-label={`${t("sites.php.remove")} ${label}`} onClick={() => remove(key)}>
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          </div>
          {option?.type === "size" ? <div className="flex min-w-0 gap-2">
            <Input id={id} type="number" min={0} step={1} disabled={disabled || unlimited} className="min-w-0 flex-1"
              value={unlimited ? "" : size?.[1] ?? value} placeholder={unlimited ? "—" : "512"}
              aria-invalid={!valid} aria-describedby={`${id}-hint ${id}-error`}
              onChange={(e) => setValue(key, `${e.target.value}${size?.[2]?.toUpperCase() ?? "M"}`)} />
            <Select value={unlimited ? "unlimited" : size?.[2]?.toUpperCase() || "bytes"} disabled={disabled}
              onValueChange={(unit) => setValue(key, unit === "unlimited" ? "-1" : `${size?.[1] || "512"}${unit === "bytes" ? "" : unit}`)}>
              <SelectTrigger className="w-auto min-w-[86px] max-w-[55%]" aria-label={`${label} ${t("sites.php.unit")}`}><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="bytes">{t("sites.php.bytes")}</SelectItem>
                <SelectItem value="K">KB</SelectItem><SelectItem value="M">MB</SelectItem><SelectItem value="G">GB</SelectItem>
                {key === "memory_limit" && <SelectItem value="unlimited">{t("sites.php.unlimited")}</SelectItem>}
              </SelectContent>
            </Select>
          </div> : option?.type === "switch" ? <div className="flex items-center gap-2">
            <Switch id={id} checked={["on", "1"].includes(value.toLowerCase())} disabled={disabled}
              aria-describedby={`${id}-hint ${id}-error`} onCheckedChange={(on) => setValue(key, on ? "On" : "Off")} />
            <span className="text-xs text-secondary">{t(["on", "1"].includes(value.toLowerCase()) ? "sites.php.on" : "sites.php.off")}</span>
          </div> : <Input id={id} type={option ? "number" : "text"} value={value} disabled={disabled} readOnly={!option}
            min={key === "max_input_time" ? -1 : ["max_input_vars", "max_file_uploads"].includes(key) ? 1 : 0}
            max={4294967295} step={1} aria-invalid={!valid} aria-describedby={`${id}-hint ${id}-error`}
            onChange={(e) => setValue(key, e.target.value)} />}
          {option && <p id={`${id}-hint`} className="text-xs leading-relaxed text-secondary">{t(hint)}</p>}
          {!option && valid && <p id={`${id}-hint`} className="text-xs leading-relaxed text-secondary">{t("sites.php.legacy")}</p>}
          <p id={`${id}-error`} tabIndex={-1} data-php-error={!valid} aria-live="polite" className="text-xs leading-relaxed text-error">{!valid && t(option ? "sites.php.invalid" : "sites.php.unsupported")}</p>
        </div>;
      })}
      {available.length > 0 && <div className="space-y-1.5">
        <Label htmlFor="site-php-add">{t("sites.php.add")}</Label>
        <Select value="" disabled={disabled} onValueChange={(key) => {
          const option = available.find((option) => option.key === key);
          if (option) { setValue(key, option.initial); requestAnimationFrame(() => document.getElementById(`site-php-${key}`)?.focus()); }
        }}>
          <SelectTrigger id="site-php-add"><SelectValue placeholder={t("sites.php.choose")} /></SelectTrigger>
          <SelectContent>{available.map((option) => <SelectItem key={option.key} value={option.key}>{t(`sites.php.${option.key}`)}</SelectItem>)}</SelectContent>
        </Select>
      </div>}
      {entries.length > 0 && <Button variant="secondary" disabled={disabled} onClick={() => {
        onChange({}); requestAnimationFrame(() => document.getElementById("site-php-add")?.focus());
      }}>{t("sites.php.clear")}</Button>}
      <p className="text-xs leading-relaxed text-secondary">{t("sites.php.preserveHint")}</p>
    </div>
  );
}
