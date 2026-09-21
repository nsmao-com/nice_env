"use client";

import * as React from "react";
import { toast } from "sonner";
import {
  FolderSearch,
  Loader2,
  Check,
  Sparkles,
  AlertTriangle,
  BadgeCheck,
  Terminal,
} from "lucide-react";
import type { ScannedProject } from "@nsb/schema";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

/**
 * 「扫描项目」：指一个父目录，把里面的项目认出来并一键建站。
 *
 * 解决的是最烦的那一步——手上已经有一堆项目目录了，却还要在建站表单里
 * 一个个手工填：站点类型、文档根、伪静态、PHP 版本。
 *
 * 识别结果**必须显示依据**（`evidence`），因为识别总会出错，让用户一眼能
 * 判断「它为什么这么认」，比给一个说不清理由的结论有用得多。
 */
export function ProjectScannerDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onCreated?: () => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const [root, setRoot] = React.useState("");
  const [scanning, setScanning] = React.useState(false);
  const [found, setFound] = React.useState<ScannedProject[] | null>(null);
  const [picked, setPicked] = React.useState<Set<string>>(new Set());
  const [creating, setCreating] = React.useState(false);

  React.useEffect(() => {
    if (!open) {
      setFound(null);
      setPicked(new Set());
    }
  }, [open]);

  const scan = () => scanFrom(root.trim());

  const scanFrom = async (dir: string) => {
    if (!dir) return;
    setScanning(true);
    try {
      const list = await api.scanProjects(dir);
      setFound(list);
      // 默认勾选「还没建过站」的，已建过的让用户自己决定
      setPicked(new Set(list.filter((p) => !p.alreadyConfigured).map((p) => p.path)));
      if (list.length === 0) toast.info(t("scanner.noneFound"));
    } catch (e) {
      toastError(e);
    } finally {
      setScanning(false);
    }
  };

  const pickFolder = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ directory: true, multiple: false, title: t("scanner.pickTitle") });
      if (typeof path === "string") {
        setRoot(path);
        // 选完目录直接开扫，省一次点击
        setTimeout(() => void scanFrom(path), 0);
      }
    } catch (e) {
      toastError(e);
    }
  };

  const createAll = async () => {
    const targets = (found ?? []).filter((p) => picked.has(p.path));
    if (targets.length === 0) return;
    setCreating(true);
    let ok = 0;
    const failed: string[] = [];
    for (const p of targets) {
      try {
        await api.createSite({
          name: p.name,
          domains: [p.suggestedDomain],
          rootDir: p.documentRoot,
          runtime: { webServer: "nginx", kind: p.siteKind as never },
          https: true,
          rewrite: p.rewrite as never,
          // 已有项目：绝不能写模板文件，否则会往用户代码里塞 index.php
          template: "none",
          // 也不写 .env.example —— 用户的 .env 配置不该被我们碰
          writeEnvExample: false,
        });
        ok += 1;
      } catch {
        failed.push(p.name);
      }
    }
    setCreating(false);
    if (failed.length === 0) {
      toast.success(t("scanner.createdN").replace("{n}", String(ok)));
    } else {
      toast.warning(
        t("scanner.createdPartial")
          .replace("{ok}", String(ok))
          .replace("{fail}", String(failed.length)),
        { description: failed.join(", "), duration: 9000 }
      );
    }
    invalidate("sites");
    onCreated?.();
    onOpenChange(false);
  };

  const toggle = (path: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(path)) n.delete(path);
      else n.add(path);
      return n;
    });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[85vh] max-w-3xl flex-col gap-0 overflow-hidden p-0">
        <DialogHeader className="shrink-0 border-b border-border px-5 py-4">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-primary/30 bg-primary-soft">
              <FolderSearch className="h-[18px] w-[18px] text-primary" strokeWidth={1.8} />
            </div>
            <div className="min-w-0 flex-1">
              <DialogTitle className="text-[15px]">{t("scanner.title")}</DialogTitle>
              <DialogDescription className="mt-0.5 text-[11.5px]">
                {t("scanner.subtitle")}
              </DialogDescription>
            </div>
          </div>
          <div className="mt-3 flex items-center gap-2">
            <Input
              value={root}
              onChange={(e) => setRoot(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void scan()}
              placeholder={t("scanner.pathPlaceholder")}
              className="h-8 font-mono text-[12px]"
            />
            <Button size="sm" variant="secondary" className="h-8 shrink-0" onClick={() => void pickFolder()}>
              {t("scanner.browse")}
            </Button>
            <Button size="sm" className="h-8 shrink-0" onClick={() => void scan()} disabled={scanning}>
              {scanning ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Sparkles className="h-3.5 w-3.5" />}
              <span className="ml-1.5">{t("scanner.scan")}</span>
            </Button>
          </div>
        </DialogHeader>

        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {scanning && (
            <div className="space-y-2">
              {Array.from({ length: 3 }).map((_, i) => (
                <div key={i} className="h-20 animate-pulse rounded-xl bg-card-2" />
              ))}
            </div>
          )}

          {!scanning && found && found.length === 0 && (
            <div className="py-12 text-center">
              <AlertTriangle className="mx-auto h-8 w-8 text-faint" strokeWidth={1.4} />
              <p className="mt-3 text-[13px] text-muted">{t("scanner.noneFound")}</p>
              <p className="mt-1 text-[11.5px] text-faint">{t("scanner.noneFoundHint")}</p>
            </div>
          )}

          {!scanning && found && found.length > 0 && (
            <div className="space-y-2">
              {found.map((p) => (
                <button
                  key={p.path}
                  type="button"
                  onClick={() => toggle(p.path)}
                  className={cn(
                    "group flex w-full items-start gap-3 rounded-xl border p-3 text-left transition-colors",
                    picked.has(p.path)
                      ? "border-primary/40 bg-primary-soft"
                      : "border-border/60 hover:border-border-strong"
                  )}
                >
                  <span
                    className={cn(
                      "mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded border",
                      picked.has(p.path)
                        ? "border-primary bg-primary text-primary-fg"
                        : "border-border-strong"
                    )}
                  >
                    {picked.has(p.path) && <Check className="h-3 w-3" strokeWidth={3} />}
                  </span>

                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-[13px] font-medium">{p.name}</span>
                      <Badge variant="outline" className="text-[10px]">
                        {p.kind}
                      </Badge>
                      {p.alreadyConfigured && (
                        <span className="inline-flex items-center gap-1 text-[10.5px] text-running">
                          <BadgeCheck className="h-3 w-3" />
                          {t("scanner.already")}
                        </span>
                      )}
                    </div>
                    <p className="mt-0.5 truncate font-mono text-[10.5px] text-faint">
                      {p.documentRoot}
                    </p>

                    {/* 识别依据：让用户能判断识别对不对，而不是盲信 */}
                    <ul className="mt-1.5 space-y-0.5">
                      {p.evidence.slice(0, 3).map((e, i) => (
                        <li key={i} className="text-[10.5px] text-muted">
                          · {e}
                        </li>
                      ))}
                    </ul>

                    <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10.5px] text-faint">
                      <span className="inline-flex items-center gap-1">
                        <Terminal className="h-3 w-3" />
                        {p.runHint}
                      </span>
                      <span className="font-mono">→ {p.suggestedDomain}</span>
                      <span className="font-mono">
                        rewrite: {p.rewrite} · {p.siteKind}
                      </span>
                      {p.phpMinVersion && (
                        <span className="font-mono text-warn">PHP {p.phpMinVersion}</span>
                      )}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}

          {!found && !scanning && (
            <div className="py-12 text-center">
              <FolderSearch className="mx-auto h-8 w-8 text-faint" strokeWidth={1.4} />
              <p className="mt-3 text-[13px] text-muted">{t("scanner.idle")}</p>
              <p className="mt-1 text-[11.5px] text-faint">{t("scanner.idleHint")}</p>
            </div>
          )}
        </div>

        {found && found.length > 0 && (
          <div className="flex shrink-0 items-center justify-between border-t border-border px-5 py-3">
            <span className="text-[11.5px] text-muted">
              {t("scanner.selected").replace("{n}", String(picked.size))}
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={() =>
                  setPicked(
                    picked.size === found.length ? new Set() : new Set(found.map((p) => p.path))
                  )
                }
              >
                {picked.size === found.length ? t("scanner.clearAll") : t("scanner.selectAll")}
              </Button>
              <Button size="sm" onClick={() => void createAll()} disabled={picked.size === 0 || creating}>
                {creating ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Sparkles className="h-3.5 w-3.5" />
                )}
                <span className="ml-1.5">
                  {t("scanner.createN").replace("{n}", String(picked.size))}
                </span>
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
