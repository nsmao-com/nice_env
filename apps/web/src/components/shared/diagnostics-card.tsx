"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  Stethoscope,
  Loader2,
  Copy,
  Check,
  Save,
  ShieldCheck,
} from "lucide-react";
import type { DiagnosticsBundle } from "@nsb/schema";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Button } from "@/components/ui/button";
import { CodeBlock } from "@/components/shared/code-block";

/**
 * 诊断包：一键汇总「报 bug 需要的全部信息」。
 *
 * 设计要点：
 * - **必须先脱敏再给人看**：密码/token 打码、用户主目录替换成 <home>。
 *   报告里明确显示「已打码 N 处」，让用户敢直接贴出去。
 * - 一次生成、可复制可另存，不用逐项截图。
 */
export function DiagnosticsCard() {
  const t = useT();
  const [bundle, setBundle] = React.useState<DiagnosticsBundle | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [copied, setCopied] = React.useState(false);

  const build = async () => {
    setLoading(true);
    try {
      const b = await api.diagnosticsBuild();
      setBundle(b);
    } catch (e) {
      toastError(e);
    } finally {
      setLoading(false);
    }
  };

  const copy = async () => {
    if (!bundle) return;
    try {
      await navigator.clipboard.writeText(bundle.markdown);
      setCopied(true);
      toast.success(t("diag.copied"));
      window.setTimeout(() => setCopied(false), 1600);
    } catch (e) {
      toastError(e);
    }
  };

  const save = async () => {
    setSaving(true);
    try {
      const p = await api.diagnosticsSave();
      toast.success(t("diag.saved"), { description: p });
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="text-[12px] leading-relaxed text-muted">{t("diag.hint")}</p>

      <div className="flex items-center gap-2">
        <Button size="sm" variant="secondary" className="h-8" onClick={() => void build()} disabled={loading}>
          {loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Stethoscope className="h-3.5 w-3.5" />}
          <span className="ml-1.5">{t("diag.generate")}</span>
        </Button>
        {bundle && (
          <>
            <Button size="sm" variant="ghost" className="h-8" onClick={() => void copy()}>
              {copied ? <Check className="h-3.5 w-3.5 text-running" /> : <Copy className="h-3.5 w-3.5" />}
              <span className="ml-1.5">{t("diag.copy")}</span>
            </Button>
            <Button size="sm" variant="ghost" className="h-8" onClick={() => void save()} disabled={saving}>
              {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
              <span className="ml-1.5">{t("diag.save")}</span>
            </Button>
          </>
        )}
      </div>

      {bundle && (
        <>
          <div className="flex flex-wrap items-center gap-3 text-[11px] text-faint">
            <span>
              {t("diag.stats")
                .replace("{s}", String(bundle.serviceCount))
                .replace("{n}", String(bundle.siteCount))
                .replace("{l}", String(bundle.logLines))}
            </span>
            {/* 明确告诉用户密码没被打进去 */}
            <span className="inline-flex items-center gap-1 text-running">
              <ShieldCheck className="h-3 w-3" />
              {t("diag.redacted").replace("{n}", String(bundle.redacted))}
            </span>
          </div>
          <CodeBlock
            code={bundle.markdown}
            lang="markdown"
            maxHeight={420}
            title={t("diag.preview")}
            compact
          />
        </>
      )}
    </div>
  );
}
