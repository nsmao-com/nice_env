"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  FileText,
  Eye,
  EyeOff,
  Loader2,
  Save,
  Wand2,
  AlertTriangle,
  Plus,
  Undo2,
} from "lucide-react";
import type { EnvFileView } from "@nsb/schema";
import { useT } from "@/lib/store";
import { toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/misc";

/**
 * 站点 .env 编辑器。
 *
 * 相比「打开文件夹改」多做三件事：
 * 1. **敏感值默认打码**（密码/token 明文摆在屏幕上不合适，尤其投屏时）
 * 2. **提示该加引号的值** —— `PASS=my pass` 不写引号会被 dotenv 截断，
 *    这个坑很难自己发现
 * 3. **一键补全 DB_*** —— 从站点绑定的数据库直接抄，不用自己去别处翻密码
 */
export function EnvEditor({ siteId }: { siteId: string }) {
  const t = useT();
  const [view, setView] = React.useState<EnvFileView | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [saving, setSaving] = React.useState(false);
  const [reveal, setReveal] = React.useState<Set<string>>(new Set());
  const [edits, setEdits] = React.useState<Record<string, string>>({});
  const [newKey, setNewKey] = React.useState("");
  const [newValue, setNewValue] = React.useState("");

  const load = React.useCallback(async () => {
    setLoading(true);
    try {
      setView(await api.envRead(siteId));
      setEdits({});
    } catch (e) {
      toastError(e);
    } finally {
      setLoading(false);
    }
  }, [siteId]);

  React.useEffect(() => {
    void load();
  }, [load]);

  const dirty = Object.keys(edits).length > 0;

  const save = async () => {
    if (!dirty) return;
    setSaving(true);
    try {
      await api.envSave(siteId, Object.entries(edits));
      toast.success(t("env.saved"), { description: t("env.backupHint") });
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  const applyDb = async () => {
    try {
      const keys = await api.envApplyDb(siteId);
      toast.success(t("env.dbApplied").replace("{n}", String(keys.length)), {
        description: keys.slice(0, 4).join(", ") + (keys.length > 4 ? " …" : ""),
      });
      await load();
    } catch (e) {
      toastError(e);
    }
  };

  const addNew = () => {
    if (!newKey.trim()) return;
    setEdits((s) => ({ ...s, [newKey.trim()]: newValue }));
    setNewKey("");
    setNewValue("");
  };

  if (loading) {
    return (
      <div className="space-y-2">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-9 w-full" />
        ))}
      </div>
    );
  }
  if (!view) return null;

  const quoteWarn = view.entries.filter((e) => e.needsQuote && !e.commented);

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <FileText className="h-3.5 w-3.5 text-faint" />
        <span className="truncate font-mono text-[11px] text-faint">{view.path}</span>
        {!view.exists && <Badge variant="outline" className="text-[9.5px]">{t("env.notExists")}</Badge>}
        <div className="ml-auto flex items-center gap-1.5">
          {view.dbHint && (
            <Button size="sm" variant="secondary" className="h-7 text-[11.5px]" onClick={() => void applyDb()}>
              <Wand2 className="h-3.5 w-3.5" />
              <span className="ml-1.5">{t("env.applyDb")}</span>
            </Button>
          )}
          <Button size="sm" className="h-7 text-[11.5px]" onClick={() => void save()} disabled={!dirty || saving}>
            {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
            <span className="ml-1.5">{t("common.save")}</span>
          </Button>
        </div>
      </div>

      {/* 需要加引号的值：这是 dotenv 最容易静默出错的地方 */}
      {quoteWarn.length > 0 && (
        <div className="flex items-start gap-2 rounded-lg border border-warn/25 bg-warn-soft px-2.5 py-2">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warn" strokeWidth={2} />
          <span className="text-[11.5px]">
            {t("env.quoteWarn").replace(
              "{keys}",
              quoteWarn.map((e) => e.key).join(", ")
            )}
          </span>
        </div>
      )}

      <div className="space-y-1">
        {view.entries.map((e) => {
          const val = edits[e.key] ?? e.value;
          const changed = e.key in edits;
          const masked = e.secret && !reveal.has(e.key);
          return (
            <div
              key={`${e.key}-${e.line}`}
              className={cn(
                "flex items-center gap-2 rounded-lg border px-2 py-1.5",
                changed ? "border-primary/40 bg-primary-soft" : "border-border/60 bg-card-2/25",
                e.commented && "opacity-60"
              )}
            >
              <span
                className="w-40 shrink-0 truncate font-mono text-[11.5px] text-muted"
                title={e.key}
              >
                {e.commented && <span className="mr-0.5 text-faint">#</span>}
                {e.key}
              </span>
              <span className="text-faint">=</span>
              <Input
                value={masked ? "••••••••" : val}
                readOnly={masked}
                onChange={(ev) =>
                  setEdits((s) => ({ ...s, [e.key]: ev.target.value }))
                }
                onFocus={() => e.secret && setReveal((s) => new Set(s).add(e.key))}
                className="h-7 flex-1 border-0 bg-transparent px-1 font-mono text-[11.5px] shadow-none focus-visible:ring-0"
              />
              {e.secret && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 shrink-0 px-1.5"
                  onClick={() =>
                    setReveal((s) => {
                      const n = new Set(s);
                      if (n.has(e.key)) n.delete(e.key);
                      else n.add(e.key);
                      return n;
                    })
                  }
                >
                  {masked ? <Eye className="h-3 w-3" /> : <EyeOff className="h-3 w-3" />}
                </Button>
              )}
            </div>
          );
        })}
      </div>

      {/* 新增变量 */}
      <div className="flex items-center gap-2 rounded-lg border border-dashed border-border px-2 py-1.5">
        <Input
          value={newKey}
          onChange={(e) => setNewKey(e.target.value.toUpperCase().replace(/[^A-Z0-9_]/g, ""))}
          placeholder={t("env.newKey")}
          className="h-7 w-40 shrink-0 px-1 font-mono text-[11.5px]"
        />
        <span className="text-faint">=</span>
        <Input
          value={newValue}
          onChange={(e) => setNewValue(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && addNew()}
          placeholder={t("env.newValue")}
          className="h-7 flex-1 px-1 font-mono text-[11.5px]"
        />
        <Button
          variant="ghost"
          size="sm"
          className="h-6 shrink-0 px-1.5"
          onClick={addNew}
          disabled={!newKey.trim()}
        >
          <Plus className="h-3.5 w-3.5" />
        </Button>
      </div>

      {dirty && (
        <div className="flex items-center gap-2 text-[11px] text-warn">
          <span>
            {t("env.pendingChanges").replace("{n}", String(Object.keys(edits).length))}
          </span>
          <Button
            variant="ghost"
            size="sm"
            className="h-6 px-1.5 text-[11px]"
            onClick={() => setEdits({})}
          >
            <Undo2 className="h-3 w-3" />
            <span className="ml-1">{t("env.discard")}</span>
          </Button>
        </div>
      )}

      {view.variants.length > 1 && (
        <p className="text-[10.5px] text-faint">
          {t("env.variants")}: {view.variants.join(" · ")}
        </p>
      )}
    </div>
  );
}
