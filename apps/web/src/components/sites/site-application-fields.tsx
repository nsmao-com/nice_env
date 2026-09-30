"use client";

import type { SiteRuntime } from "@nsb/schema";
import { FolderOpen, Plus, Trash2 } from "lucide-react";
import { useT } from "@/lib/store";
import { isTauri } from "@/lib/backend";
import { applicationRuntime, sameVersion } from "@/lib/utils";
import { toastError } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export function SiteApplicationFields({ id, kind, value, versions, rootDir, disabled, locked, onChange }: {
  id: string;
  kind: SiteRuntime["kind"];
  value: SiteRuntime["application"];
  versions: string[];
  rootDir: string;
  disabled?: boolean;
  locked?: boolean;
  onChange: (value: SiteRuntime["application"]) => void;
}) {
  const t = useT();
  const runtime = applicationRuntime(kind);
  if (!runtime) return null;
  const blocked = disabled || locked;
  const pick = async (index?: number) => {
    if (!isTauri || !value || blocked) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ directory: index === undefined, multiple: false, title: t(index === undefined ? "appProcess.cwd" : "appProcess.pickFile") });
      if (typeof path !== "string") return;
      onChange(index === undefined ? { ...value, cwd: path } : { ...value, args: value.args.map((arg, i) => i === index ? path : arg) });
    } catch (error) { toastError(error); }
  };
  return (
    <div className="min-w-0 space-y-4">
      <div className="flex items-start justify-between gap-4 rounded-xl bg-fill p-3.5">
        <div className="min-w-0 space-y-1">
          <Label htmlFor={`${id}-managed`}>{t("appProcess.manage")}</Label>
          <p className="text-xs leading-relaxed text-muted">{t(value ? "appProcess.managedHint" : "appProcess.externalHint")}</p>
        </div>
        <Switch id={`${id}-managed`} checked={!!value} disabled={blocked}
          onCheckedChange={(enabled) => onChange(enabled ? { version: versions[0] ?? "", args: [...runtime.args] } : undefined)} />
      </div>
      {locked && <p className="text-xs leading-relaxed text-muted">{t("appProcess.stopBeforeEdit")}</p>}
      {value && <>
        {!isTauri && <p className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t("appProcess.demo")}</p>}
        <div className="space-y-1.5">
          <Label htmlFor={`${id}-version`}>{runtime.label} · {t("appProcess.version")}</Label>
          <Select value={value.version || undefined} disabled={blocked || !versions.length} onValueChange={(version) => onChange({ ...value, version })}>
            <SelectTrigger id={`${id}-version`} aria-invalid={!versions.some((candidate) => sameVersion(candidate, value.version))}><SelectValue placeholder={t("appProcess.chooseVersion")} /></SelectTrigger>
            <SelectContent>
          {!!value.version && !versions.some((candidate) => sameVersion(candidate, value.version)) && <SelectItem value={value.version} disabled>{value.version} · {t("appProcess.missing")}</SelectItem>}
              {versions.map((version) => <SelectItem key={version} value={version}>{runtime.label} {version}</SelectItem>)}
            </SelectContent>
          </Select>
          {!versions.some((candidate) => sameVersion(candidate, value.version)) && <p role="alert" className="text-xs leading-relaxed text-error">{t("appProcess.installFirst")}</p>}
          <p className="text-xs leading-relaxed text-muted">{t("appProcess.versionHint")}</p>
        </div>
        <div className="space-y-2">
          <Label>{t("appProcess.arguments")}</Label>
          <p className="text-xs leading-relaxed text-muted">{t("appProcess.argumentsHint").replace("{runtime}", runtime.label)}</p>
          {value.args.map((arg, index) => <div key={index} className="flex min-w-0 items-center gap-1.5">
            <Input value={arg} disabled={blocked} aria-label={`${t("appProcess.argument")} ${index + 1}`}
              className="min-w-0 flex-1 font-mono text-xs" spellCheck={false}
              onChange={(event) => onChange({ ...value, args: value.args.map((item, i) => i === index ? event.target.value : item) })} />
            {isTauri && <Button type="button" variant="ghost" size="icon" disabled={blocked} title={t("appProcess.pickFile")} aria-label={`${t("appProcess.pickFile")} ${index + 1}`} onClick={() => pick(index)}><FolderOpen className="size-4" /></Button>}
            <Button type="button" variant="ghost" size="icon" disabled={blocked || value.args.length <= 1} aria-label={`${t("appProcess.removeArgument")} ${index + 1}`}
              onClick={() => onChange({ ...value, args: value.args.filter((_, i) => i !== index) })}><Trash2 className="size-3.5" /></Button>
          </div>)}
          <Button type="button" variant="secondary" size="sm" disabled={blocked || value.args.length >= 64} onClick={() => onChange({ ...value, args: [...value.args, ""] })}><Plus className="size-3.5" />{t("appProcess.addArgument")}</Button>
        </div>
        <details className="group rounded-xl border border-border px-3.5 py-3">
          <summary className="cursor-pointer text-xs font-medium">{t("appProcess.cwd")}</summary>
          <div className="mt-3 space-y-2">
            <Label htmlFor={`${id}-cwd`} className="text-xs">{t("appProcess.cwdHint")}</Label>
            <div className="flex gap-2">
              <Input id={`${id}-cwd`} value={value.cwd ?? ""} placeholder={rootDir} disabled={blocked} className="min-w-0 flex-1 font-mono text-xs"
                onChange={(event) => onChange({ ...value, cwd: event.target.value || undefined })} />
              {isTauri && <Button type="button" variant="secondary" size="icon" disabled={blocked} aria-label={t("appProcess.cwd")} onClick={() => pick()}><FolderOpen className="size-4" /></Button>}
            </div>
          </div>
        </details>
        <p className="text-xs leading-relaxed text-muted">{t("appProcess.portHint")}</p>
      </>}
    </div>
  );
}
