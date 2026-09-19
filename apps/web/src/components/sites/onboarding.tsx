"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import { Globe, Rocket, Database, Boxes, Check, ChevronRight, Sparkles } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { usePackages, useInvalidate, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { RingProgress } from "@/components/shared/ring-progress";
import { cn, fmtSpeed } from "@/lib/utils";
import type { DownloadProgress } from "@nsb/schema";
import { listen } from "@/lib/backend";

const SCENES = [
  {
    id: "php",
    icon: Globe,
    titleKey: "ob.scenePhp",
    hintKey: "ob.scenePhpHint",
    packages: ["nginx", "php", "mysql", "redis"],
  },
  {
    id: "frontend",
    icon: Sparkles,
    titleKey: "ob.sceneFrontend",
    hintKey: "ob.sceneFrontendHint",
    packages: ["nginx"],
  },
  {
    id: "db",
    icon: Database,
    titleKey: "ob.sceneDb",
    hintKey: "ob.sceneDbHint",
    packages: ["mysql", "redis"],
  },
] as const;

/** 首次启动引导：选场景 → 自动装套件 → 创建站点 */
export function Onboarding() {
  const t = useT();
  const [open, setOpen] = React.useState(false);
  const [scene, setScene] = React.useState<string | null>(null);
  const [phase, setPhase] = React.useState<"pick" | "installing" | "done">("pick");
  const [taskProgress, setTaskProgress] = React.useState<Record<string, DownloadProgress>>({});
  const [installErrors, setInstallErrors] = React.useState<string[]>([]);
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const invalidate = useInvalidate();

  /* 首启才弹：未完成引导 且 还没装过任何套件；已有套件则静默标记完成，绝不打扰 */
  React.useEffect(() => {
    let alive = true;
    Promise.all([
      api.getSettings().catch(() => undefined),
      api.listPackages().catch(() => undefined),
    ])
      .then(([s, pkgs]) => {
        if (!alive || !s) return;
        const hasInstalled = (pkgs ?? []).some((p) => p.install);
        if (s.onboardingDone) return;
        if (hasInstalled) {
          api.setSetting("onboardingDone", true).catch(() => undefined);
          return;
        }
        setOpen(true);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, []);

  React.useEffect(() => {
    let un: (() => void) | undefined;
    listen<DownloadProgress>("download://progress", (p) => {
      setTaskProgress((prev) => ({ ...prev, [p.taskId]: p }));
    }).then((u) => (un = u));
    return () => un?.();
  }, []);

  const { data: packages } = usePackages();

  const installPkgId = (sceneId: string) => {
    const sc = SCENES.find((s) => s.id === sceneId);
    if (!sc) return [];
    /* PHP 场景挑最新 php 版本条目 */
    if (sc.id === "php") {
      const phps = packages.filter((p) => p.id === "php");
      const newest = phps.sort((a, b) => b.version.localeCompare(a.version))[0];
      return newest ? ["nginx", newest.id, "mysql", "redis"].filter((v, i, a) => a.indexOf(v) === i) : sc.packages.slice();
    }
    return [...sc.packages];
  };

  const startInstall = async () => {
    setPhase("installing");
    const ids = installPkgId(scene!);
    const errors: string[] = [];
    for (const id of ids) {
      try {
        await api.installPackage(id);
      } catch (e) {
        errors.push(`${id}: ${String((e as Error).message ?? e)}`);
      }
    }
    setInstallErrors(errors);
    invalidate("packages", "services");
    setPhase("done");
    if (errors.length === 0) toast.success(t("ob.suiteDone"));
  };

  const finish = async (createSite: boolean) => {
    try {
      await api.setSetting("onboardingDone", true);
    } catch {
      /* ignore */
    }
    setOpen(false);
    if (createSite) setWizardOpen(true);
  };

  if (!open) return null;

  const ids = scene ? installPkgId(scene) : [];

  return (
    <Dialog open={open} onOpenChange={(o) => !o && finish(false)}>
      <DialogContent className="max-w-[600px]">
        <DialogTitle className="sr-only">{t("ob.srTitle")}</DialogTitle>
        <AnimatePresence mode="wait">
          {phase === "pick" && (
            <motion.div key="pick" initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -10 }} className="flex flex-col gap-5">
              <div className="flex flex-col items-center gap-1.5 pt-4">
                <motion.div
                  initial={{ scale: 0.8, opacity: 0 }}
                  animate={{ scale: 1, opacity: 1 }}
                  transition={{ type: "spring", stiffness: 300, damping: 20 }}
                  className="flex h-12 w-12 items-center justify-center rounded-2xl bg-primary text-primary-fg shadow-sm"
                >
                  <Rocket className="h-6 w-6 text-primary-fg" />
                </motion.div>
                <h2 className="text-lg font-semibold">{t("onboarding.welcome")}</h2>
                <p className="text-[13px] text-muted">{t("onboarding.subtitle")}</p>
              </div>

              <div className="grid grid-cols-3 gap-3">
                {SCENES.map((s) => (
                  <button
                    key={s.id}
                    onClick={() => setScene(s.id)}
                    className={cn(
                      "flex flex-col items-center gap-2 rounded-xl border p-4 transition-all",
                      scene === s.id
                        ? "border-primary/60 bg-primary-soft"
                        : "border-border hover:border-border-strong bg-card-2/40"
                    )}
                  >
                    <s.icon className={cn("h-5 w-5", scene === s.id ? "text-primary" : "text-faint")} />
                    <span className="text-[12.5px] font-medium">{t(s.titleKey)}</span>
                    <span className="text-center text-[10.5px] leading-tight text-faint">{t(s.hintKey)}</span>
                  </button>
                ))}
              </div>

              <div className="flex items-center justify-between">
                <Button variant="ghost" size="sm" onClick={() => finish(false)}>
                  {t("ob.skip")}
                </Button>
                <Button disabled={!scene} onClick={startInstall}>
                  {t("ob.installBtn")} <ChevronRight className="h-3.5 w-3.5" />
                </Button>
              </div>
            </motion.div>
          )}

          {phase === "installing" && (
            <motion.div key="installing" initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="flex flex-col gap-4">
              <div className="flex flex-col items-center gap-1.5 pt-4">
                <RingProgress value={0} indeterminate size={56}>
                  <Boxes className="h-5 w-5 text-primary" />
                </RingProgress>
                <h2 className="text-[15px] font-semibold">{t("onboarding.installing")}</h2>
                <p className="text-xs text-faint">{t("ob.stepsHint")}</p>
              </div>
              <div className="flex max-h-56 flex-col gap-2 overflow-y-auto rounded-xl border border-border bg-card-2/30 p-3 font-mono text-[11px]">
                {ids.map((id) => (
                  <div key={id} className="flex items-center justify-between text-secondary">
                    <span>{id}</span>
                    <span className="text-faint">{t("ob.installingEach")}</span>
                  </div>
                ))}
              </div>
              <p className="text-center text-[11px] text-faint">{t("ob.bigFileHint")}</p>
              <div className="flex justify-center">
                <Button variant="ghost" size="sm" onClick={() => finish(false)}>
                  {t("ob.bgInstall")}
                </Button>
              </div>
            </motion.div>
          )}

          {phase === "done" && (
            <motion.div key="done" initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} className="flex flex-col gap-5">
              <div className="flex flex-col items-center gap-1.5 pt-4">
                <motion.div
                  initial={{ scale: 0 }}
                  animate={{ scale: 1 }}
                  transition={{ type: "spring", stiffness: 300, damping: 18 }}
                  className="flex h-12 w-12 items-center justify-center rounded-full bg-running-soft"
                >
                  <Check className="h-6 w-6 text-running" strokeWidth={2.5} />
                </motion.div>
                <h2 className="text-lg font-semibold">{t("ob.doneTitle")}</h2>
                <p className="text-[13px] text-muted">{t("ob.nowCreate")}</p>
              </div>
              {installErrors.length > 0 && (
                <div className="rounded-xl border border-warn/30 bg-warn/10 p-3 text-[11.5px] text-warn">
                  {t("ob.suiteFailed")}
                  {installErrors.map((e) => (
                    <div key={e} className="font-mono">{e}</div>
                  ))}
                  {t("ob.retryInPkgs")}
                </div>
              )}
              <div className="flex items-center justify-between">
                <Button variant="ghost" onClick={() => finish(false)}>
                  {t("ob.later")}
                </Button>
                <Button onClick={() => finish(true)}>
                  <Globe className="h-3.5 w-3.5" /> {t("onboarding.createSite")}
                </Button>
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </DialogContent>
    </Dialog>
  );
}
