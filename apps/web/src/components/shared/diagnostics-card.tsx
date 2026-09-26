"use client";

import * as React from "react";
import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Stethoscope, Loader2, Copy, Save, ShieldCheck } from "lucide-react";
import type { DiagnosticsBundle } from "@nsb/schema";
import { useT } from "@/lib/store";
import { copyText } from "@/lib/hooks";
import { isTauri, normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { CodeBlock } from "@/components/shared/code-block";

/** 预览、复制和保存使用同一份快照；采集失败时保留已有报告。 */
export function DiagnosticsCard() {
  const t = useT();
  const client = useQueryClient();
  const report = useQuery({ queryKey: ["diagnostics-report"], queryFn: api.diagnosticsBuild, enabled: false, retry: false });
  const bundle = report.data;
  const busy = useIsMutating({ mutationKey: ["diagnostics"] }) > 0;
  const building = useIsMutating({ mutationKey: ["diagnostics", "build"] }) > 0;
  const build = useMutation({
    mutationKey: ["diagnostics", "build"],
    mutationFn: api.diagnosticsBuild,
    onSuccess: (next) => client.setQueryData<DiagnosticsBundle>(["diagnostics-report"], next),
  });
  const save = useMutation({ mutationKey: ["diagnostics", "save"], mutationFn: api.diagnosticsSave });
  const [error, setError] = React.useState<string | null>(null);
  const [saved, setSaved] = React.useState<string | null>(null);
  const action = React.useRef(false);
  const errorRef = React.useRef<HTMLParagraphElement>(null);
  const reportRef = React.useRef<HTMLDivElement>(null);
  const run = async (kind: "build" | "save") => {
    if (action.current || client.isMutating({ mutationKey: ["diagnostics"] })) return;
    if (kind === "save" && !bundle) return;
    action.current = true;
    setError(null);
    setSaved(null);
    try {
      if (kind === "build") {
        await build.mutateAsync();
        requestAnimationFrame(() => reportRef.current?.focus());
      } else if (bundle) {
        const path = await save.mutateAsync(bundle);
        setSaved(path);
        toast.success(t(isTauri ? "diag.saved" : "diag.downloaded"), { description: path });
      }
    } catch (cause) {
      setError(normalizeError(cause).message);
      requestAnimationFrame(() => errorRef.current?.focus());
    } finally {
      action.current = false;
    }
  };

  return <div className="flex min-w-0 flex-col gap-3">
    <div className="flex flex-wrap items-center gap-2">
      <Button size="sm" variant="secondary" className="min-h-9 h-auto whitespace-normal py-2" onClick={() => void run("build")} disabled={busy}>
        {building ? <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" /> : <Stethoscope className="h-3.5 w-3.5 shrink-0" />}
        {t(bundle ? "diag.regenerate" : "diag.generate")}
      </Button>
      {bundle && <>
        <Button size="sm" variant="ghost" className="min-h-9" onClick={() => void copyText(bundle.markdown)} disabled={busy}>
          <Copy className="h-3.5 w-3.5 shrink-0" />{t("diag.copy")}
        </Button>
        <Button size="sm" variant="ghost" className="min-h-9 h-auto whitespace-normal py-2" onClick={() => void run("save")} disabled={busy}>
          {save.isPending ? <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" /> : <Save className="h-3.5 w-3.5 shrink-0" />}
          {t(isTauri ? "diag.save" : "diag.download")}
        </Button>
      </>}
    </div>
    <p className="text-[11px] leading-relaxed text-muted">{t(isTauri ? "diag.snapshotHint" : "diag.demoHint")}</p>
    {busy && <p role="status" className="text-[11px] text-muted">{t(building ? "diag.building" : "diag.saving")}</p>}
    {error && <p ref={errorRef} tabIndex={-1} role="alert" className="break-words rounded-md border border-error/30 p-3 text-xs text-error">{error}</p>}
    {saved && <p role="status" className="break-all text-[11px] text-muted">{t(isTauri ? "diag.saved" : "diag.downloaded")}：{saved}</p>}
    {bundle && <div ref={reportRef} tabIndex={-1} aria-label={t("diag.preview")} className="min-w-0 space-y-3 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-primary">
      <div className="flex flex-wrap gap-x-3 gap-y-2 text-[11px] text-muted">
        <span>{t("diag.stats").replace("{s}", String(bundle.serviceCount)).replace("{n}", String(bundle.siteCount)).replace("{l}", String(bundle.logLines))}</span>
        <span className="inline-flex items-center gap-1"><ShieldCheck className="h-3 w-3 shrink-0" />{t("diag.redacted").replace("{n}", String(bundle.redacted))}</span>
        <time dateTime={new Date(bundle.generatedAt * 1000).toISOString()}>{t("diag.generatedAt")} {new Date(bundle.generatedAt * 1000).toLocaleString()}</time>
      </div>
      <p className="text-[11px] leading-relaxed text-muted">{t("diag.reviewHint")}</p>
      {bundle.warnings.length > 0 && <details className="rounded-md border border-warn/30 p-3" open>
        <summary className="cursor-pointer text-xs text-warn">{t("diag.warnings").replace("{n}", String(bundle.warnings.length))}</summary>
        <ul className="mt-2 space-y-2 text-[11px] leading-relaxed text-muted">
          {bundle.warnings.map((warning, i) => <li key={i} className="break-words border-t border-dashed border-border pt-2">{warning}</li>)}
        </ul>
      </details>}
      <CodeBlock code={bundle.markdown} lang="markdown" maxHeight={420} title={t("diag.preview")} compact />
    </div>}
  </div>;
}
