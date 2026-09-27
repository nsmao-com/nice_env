"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
import { Globe, Rocket, Database, Boxes, Check, ChevronRight, Sparkles, AlertTriangle, Loader2 } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { usePackages, useSettings, useInvalidate } from "@/lib/hooks";
import { useInstallTasks, progressForTask, type InstallStatus } from "@/lib/install-tasks";
import { normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { RingProgress } from "@/components/shared/ring-progress";
import { cn, cmpVersionDesc, isPlatformCompatible } from "@/lib/utils";

const SCENES = [
  { id: "php", icon: Globe, titleKey: "ob.scenePhp", hintKey: "ob.scenePhpHint", packages: ["nginx", "php", "mysql", "redis"] },
  { id: "frontend", icon: Sparkles, titleKey: "ob.sceneFrontend", hintKey: "ob.sceneFrontendHint", packages: ["nginx", "node"] },
  { id: "db", icon: Database, titleKey: "ob.sceneDb", hintKey: "ob.sceneDbHint", packages: ["mysql", "redis"] },
] as const;

const PACKAGE_NAMES: Record<string, string> = { nginx: "Nginx", php: "PHP", mysql: "MySQL", redis: "Redis", node: "Node.js" };
type SetupItem = { id: string; status: InstallStatus | "queued"; version?: string; error?: string };
type NextAction = "site" | "packages" | "databases";

/** 保留本次安装结果；只有运行中的项目读取实时任务，避免重试时借用旧状态。 */
function InstallingList({ items }: { items: SetupItem[] }) {
  const t = useT();
  const tasks = useInstallTasks((s) => s.tasks);
  const progress = useInstallTasks((s) => s.progress);
  const cancel = useInstallTasks((s) => s.cancel);
  return <ul aria-label={t("ob.packageList")} className="rounded-xl bg-fill px-3">
    {items.map((item) => {
      const task = item.status === "running" ? tasks[item.id] : undefined;
      const p = progressForTask(progress, task);
      const status = task?.status ?? item.status;
      const version = task?.resolvedVersion ?? item.version;
      const pct = p && p.total > 0 ? Math.min(100, Math.max(0, Math.round(p.received / p.total * 100))) : null;
      const label = status === "done" ? t("install.stage.done") : status === "error" ? t("install.failed")
        : status === "cancelled" ? t("install.cancelled") : status === "queued" ? t("ob.queued")
        : task?.cancelRequested ? t("install.cancelling") : p?.state === "verifying" || p?.state === "downloaded" ? t("install.stage.verify")
        : p?.state === "extracting" ? t("install.stage.extract") : p?.state === "configuring" || p?.state === "installed" ? t("install.stage.config")
        : pct !== null ? `${pct}%` : t("ob.installingEach");
      return <li key={item.id} className="space-y-1 border-t border-dashed border-separator py-3 first:border-t-0">
        <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
          <span className="font-medium">{PACKAGE_NAMES[item.id] ?? item.id}{version && <span className="ml-2 font-mono text-muted">{version}</span>}</span>
          <span className={cn("tabular", status === "done" ? "text-running" : status === "error" ? "text-error" : "text-muted")}>{label}</span>
          {status === "running" && <Button size="sm" variant="ghost" aria-label={`${t("install.cancel")} ${PACKAGE_NAMES[item.id] ?? item.id}`}
            disabled={task?.cancelRequested || p?.state === "configuring" || p?.state === "installed"} onClick={() => void cancel(item.id)}>{t("install.cancel")}</Button>}
        </div>
        {item.error && <p className="whitespace-pre-wrap text-xs leading-relaxed text-error [overflow-wrap:anywhere]">{item.error}</p>}
      </li>;
    })}
  </ul>;
}

/** 首次启动引导：选场景 → 准备套件 → 按场景进入站点或数据库。 */
export function Onboarding() {
  const t = useT();
  const router = useRouter();
  const packageQuery = usePackages();
  const settingsQuery = useSettings();
  const invalidate = useInvalidate();
  const startTask = useInstallTasks((s) => s.start);
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const [open, setOpen] = React.useState(false);
  const [scene, setScene] = React.useState<(typeof SCENES)[number]["id"] | null>(null);
  const [phase, setPhase] = React.useState<"pick" | "installing" | "done">("pick");
  const [items, setItems] = React.useState<SetupItem[]>([]);
  const [batchError, setBatchError] = React.useState("");
  const [saveError, setSaveError] = React.useState("");
  const [saving, setSaving] = React.useState(false);
  const checked = React.useRef(false);
  const running = React.useRef(false);
  const savingRef = React.useRef(false);
  const nextAction = React.useRef<NextAction | undefined>(undefined);
  const failedAction = React.useRef<NextAction | undefined>(undefined);
  const bodyRef = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    bodyRef.current?.scrollTo({ top: 0 });
  }, [saveError, phase]);

  // 必须成功读到真实套件列表；读取失败或初始空缓存都不等于“未安装”。
  React.useEffect(() => {
    if (checked.current || !settingsQuery.data || settingsQuery.isError || packageQuery.isError || !packageQuery.dataUpdatedAt) return;
    checked.current = true;
    if (!settingsQuery.data.onboardingDone && !packageQuery.data.some((p) => p.install)) setOpen(true);
  }, [settingsQuery.data, settingsQuery.isError, packageQuery.data, packageQuery.dataUpdatedAt, packageQuery.isError]);

  const selected = SCENES.find((s) => s.id === scene);
  const missing = selected?.packages.filter((id) => !packageQuery.data.some((p) => p.id === id && (p.install || isPlatformCompatible(p.os, p.arch)))) ?? [];
  const ready = phase === "done" && items.length > 0 && items.every((item) => item.status === "done") && !batchError;
  const completed = items.filter((item) => item.status === "done").length;
  const readFailed = packageQuery.isError || settingsQuery.isError;

  const startInstall = async () => {
    if (!selected || running.current || savingRef.current) return;
    running.current = true;
    const plan: SetupItem[] = phase === "pick" ? selected.packages.map((id) => ({ id, status: "queued" }))
      : items.map((item) => item.status === "done" ? item : { id: item.id, status: "queued" });
    const publish = () => setItems(plan.map((item) => ({ ...item })));
    setBatchError("");
    setSaveError("");
    setPhase("installing");
    publish();
    let readError = false;
    try {
      for (const item of plan) {
        if (item.status === "done") continue;
        // 每次发起前重新读取，复用已安装套件，避免在后台已完成后再次安装。
        const available = (await api.listPackages()).filter((p) => p.id === item.id);
        const installed = available.filter((p) => p.install).sort((a, b) => cmpVersionDesc(a.version, b.version))[0];
        if (installed) {
          item.status = "done";
          item.version = installed.version;
          publish();
          continue;
        }
        if (!available.some((p) => isPlatformCompatible(p.os, p.arch))) {
          item.status = "error";
          item.error = t("ob.packageUnavailable").replace("{name}", PACKAGE_NAMES[item.id] ?? item.id);
          publish();
          continue;
        }
        item.status = "running";
        publish();
        const ok = await startTask({ id: item.id, displayName: PACKAGE_NAMES[item.id] ?? item.id }, { quiet: true });
        const task = useInstallTasks.getState().tasks[item.id];
        item.status = ok ? "done" : task?.status === "cancelled" ? "cancelled" : "error";
        item.version = task?.resolvedVersion;
        item.error = item.status === "error" ? task?.error ?? t("install.failed") : undefined;
        publish();
      }
    } catch (e) {
      readError = true;
      const message = `${t("ob.readFailed")} ${normalizeError(e).message}`;
      setBatchError(message);
      toast.error(t("ob.incompleteTitle"), {
        description: message,
        classNames: { description: "line-clamp-2 [overflow-wrap:anywhere]" },
        action: { label: t("install.viewTask"), onClick: () => setOpen(true) },
      });
    } finally {
      running.current = false;
      invalidate("packages", "services");
      setPhase("done");
    }
    if (!readError && plan.length && plan.every((item) => item.status === "done")) toast.success(t("ob.suiteDone"));
  };

  const finish = async (next?: NextAction) => {
    if (savingRef.current) return;
    savingRef.current = true;
    setSaving(true);
    setSaveError("");
    try {
      await api.setSetting("onboardingDone", true);
      invalidate("settings");
      nextAction.current = next;
      setOpen(false);
    } catch (e) {
      failedAction.current = next;
      setSaveError(`${t("ob.saveFailed")} ${normalizeError(e).message}`);
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };

  const heading = phase === "pick" ? t("onboarding.welcome") : phase === "installing" ? t("onboarding.installing")
    : ready ? t("ob.doneTitle") : t("ob.incompleteTitle");
  const description = phase === "pick" ? t("ob.chooseHint") : phase === "installing" ? t("ob.stepsHint")
    : !ready ? t("ob.incompleteHint") : scene === "db" ? t("ob.databaseReadyHint") : scene === "frontend" ? t("ob.frontendReadyHint") : t("ob.nowCreate");

  return <Dialog open={open} onOpenChange={(value) => { if (!value) void finish(); }}>
    <DialogContent hideClose={saving} className="flex max-h-[calc(100dvh-24px)] max-w-[600px] flex-col gap-0 overflow-hidden p-0"
      onCloseAutoFocus={(event) => {
        const next = nextAction.current;
        nextAction.current = undefined;
        if (!next) return;
        event.preventDefault();
        if (next === "site") setWizardOpen(true, scene === "frontend" ? "static" : "php");
        else router.push(next === "databases" ? "/databases" : "/packages");
      }}>
      <div className="shrink-0 px-4 py-4 pr-12 sm:px-6 sm:pr-12">
        <div className="flex items-center gap-3">
          <div className={cn("flex h-10 w-10 shrink-0 items-center justify-center rounded-xl", phase === "done" ? ready ? "bg-running-soft text-running" : "bg-warn/10 text-warn" : "bg-primary-soft text-primary")}>
            {phase === "pick" ? <Rocket className="h-5 w-5" /> : phase === "installing" ? <Loader2 className="h-5 w-5 animate-spin motion-reduce:animate-none" /> : ready ? <Check className="h-5 w-5" /> : <AlertTriangle className="h-5 w-5" />}
          </div>
          <div className="min-w-0">
            <DialogTitle className="text-[15px] leading-snug">{heading}</DialogTitle>
          </div>
        </div>
      </div>
      <div role="separator" className="mx-4 shrink-0 border-t border-dashed border-separator sm:mx-6" />
      <div ref={bodyRef} className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 py-4 sm:px-6">
        {saveError && <div className="space-y-2">
          <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{saveError}</p>
          <Button size="sm" variant="outline" disabled={saving} onClick={() => { nextAction.current = failedAction.current; setOpen(false); }}>{t("ob.continueWithoutSaving")}</Button>
        </div>}
        <DialogDescription className="text-xs leading-relaxed">{description}</DialogDescription>
        {batchError && <p role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{batchError}</p>}
        {phase === "pick" ? <>
          <div className="grid gap-3 sm:grid-cols-3" role="group" aria-label={t("onboarding.scene")}>
            {SCENES.map((option) => <button key={option.id} type="button" aria-pressed={scene === option.id} onClick={() => setScene(option.id)}
              className={cn("flex items-center gap-3 rounded-xl border p-3 text-left transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary sm:flex-col sm:items-start", scene === option.id ? "border-primary/60 bg-primary-soft" : "border-border bg-card-2/40 hover:border-border-strong")}>
              <option.icon className={cn("h-5 w-5 shrink-0", scene === option.id ? "text-primary" : "text-muted")} />
              <span className="min-w-0 space-y-1"><span className="block text-[12.5px] font-medium">{t(option.titleKey)}</span><span className="block text-xs leading-relaxed text-muted">{t(option.hintKey)}</span></span>
            </button>)}
          </div>
          {selected && <p className="text-xs leading-relaxed text-secondary">{t("ob.willInstall")} {selected.packages.map((id) => PACKAGE_NAMES[id]).join(" + ")}</p>}
          {missing.length > 0 && <p role="alert" className="text-xs text-warn [overflow-wrap:anywhere]">{t("ob.unavailableHint").replace("{names}", missing.map((id) => PACKAGE_NAMES[id]).join("、"))}</p>}
          {readFailed && <div role="alert" className="space-y-2 text-xs text-error"><p>{t("ob.readFailed")}</p><Button size="sm" variant="outline" onClick={() => { void packageQuery.refetch(); void settingsQuery.refetch(); }}>{t("install.retry")}</Button></div>}
        </> : <>
          <div className="flex items-center gap-3" role="status">
            {phase === "installing" && <RingProgress value={items.length ? completed / items.length * 100 : 0} size={40}><Boxes className="h-4 w-4 text-primary" /></RingProgress>}
            <p className="text-xs text-muted">{t("ob.completedCount").replace("{done}", String(completed)).replace("{total}", String(items.length))}</p>
          </div>
          <InstallingList items={items} />
          {phase === "installing" && <p className="text-xs leading-relaxed text-muted">{t("ob.backgroundHint")}</p>}
        </>}
      </div>
      <div role="separator" className="mx-4 shrink-0 border-t border-dashed border-separator sm:mx-6" />
      <div className="shrink-0 space-y-2 px-4 py-3 sm:px-6">
        {saving && <p role="status" className="text-xs text-muted">{t("detail.saving")}</p>}
        <div className="flex flex-wrap items-center justify-end gap-2">
          {phase === "pick" ? <>
            <Button variant="ghost" disabled={saving} onClick={() => void finish(missing.length ? "packages" : undefined)}>{t(missing.length ? "ob.managePackages" : "ob.skip")}</Button>
            <Button disabled={!scene || missing.length > 0 || readFailed || saving} onClick={() => void startInstall()}>{t("ob.installBtn")}<ChevronRight className="h-3.5 w-3.5" /></Button>
          </> : phase === "installing" ? <Button variant="outline" disabled={saving} onClick={() => void finish("packages")}>{t("ob.bgInstall")}</Button>
            : <>
              <Button variant="ghost" disabled={saving} onClick={() => void finish(ready ? undefined : "packages")}>{t(ready ? "ob.later" : "ob.managePackages")}</Button>
              {!ready ? <Button disabled={saving} onClick={() => void startInstall()}>{t("ob.retryIncomplete")}</Button>
                : <Button disabled={saving} onClick={() => void finish(scene === "db" ? "databases" : "site")}>{t(scene === "db" ? "ob.manageDatabases" : "onboarding.createSite")}</Button>}
            </>}
        </div>
      </div>
    </DialogContent>
  </Dialog>;
}
