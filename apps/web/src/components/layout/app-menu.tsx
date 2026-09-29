"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { version as bundledAppVersion } from "../../../package.json";
import {
  ChevronDown,
  RefreshCw,
  Github,
  Plus,
  Rocket,
  Square,
  Settings,
  FolderOpen,
  Info,
  LogOut,
  EyeOff,
  ExternalLink,
} from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { useServices, useStacks, toastError, useQuickServiceActions } from "@/lib/hooks";
import * as api from "@/lib/api";
import { isTauri, listen, normalizeError } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { ConfirmDialog } from "@/components/shared/misc";
import { UpdateDialog } from "@/components/shared/update-dialog";
import { BulkResult } from "@/components/shared/bulk-actions";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuLabel,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { useDesktopWindow } from "./titlebar";

const REPO_URL = "https://github.com/nsmao-com/nice_env";
const RELEASES_URL = `${REPO_URL}/releases`;

export function AppLogo({ className }: { className?: string }) {
  return (
    <div
      className={cn(
        "relative flex h-7 w-7 shrink-0 items-center justify-center rounded-[9px] bg-primary text-primary-fg shadow-sm",
        className
      )}
    >
      <svg
        viewBox="0 0 24 24"
        className="h-4 w-4 text-primary-fg"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M4 7l8-4 8 4-8 4z" />
        <path d="M4 12l8 4 8-4" />
        <path d="M4 17l8 4 8-4" />
      </svg>
    </div>
  );
}

export function AppMenu({ collapsed }: { collapsed: boolean }) {
  const t = useT();
  const router = useRouter();
  const setWizardOpen = useUI((s) => s.setWizardOpen);
  const serviceQuery = useServices();
  const stackQuery = useStacks();
  const services = serviceQuery.data;
  const stacks = stackQuery.data;
  const quickStackId = useUI((s) => s.quickStackId);
  const selectedStack = stacks.find((stack) => stack.id === quickStackId) ?? stacks[0];
  const servicesReady = serviceQuery.dataUpdatedAt > 0 && !serviceQuery.error;
  const startReady = servicesReady && stackQuery.dataUpdatedAt > 0 && !stackQuery.error;
  const quick = useQuickServiceActions(services, stacks);
  const { isMac, minimize, close } = useDesktopWindow();
  const [aboutOpen, setAboutOpen] = React.useState(false);
  const [checking, setChecking] = React.useState(false);
  const [version, setVersion] = React.useState(bundledAppVersion);
  const [updateOpen, setUpdateOpen] = React.useState(false);
  const [confirm, setConfirm] = React.useState<null | "stopAll" | "quit">(null);
  const [busy, setBusy] = React.useState(false);
  const quitBusy = React.useRef(false);
  const [quitError, setQuitError] = React.useState<string | null>(null);
  const quitErrorRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => { if (quitError) quitErrorRef.current?.focus(); }, [quitError]);

  React.useEffect(() => {
    api.getAppVersion().then(setVersion).catch(() => undefined);
  }, []);

  /* 托盘「检查更新…」「关于」→ Rust 发事件，这里打开对应弹窗 */
  React.useEffect(() => {
    const uns: (() => void)[] = [];
    listen("tray://check-update", () => setUpdateOpen(true)).then((u) => uns.push(u));
    listen("tray://about", () => setAboutOpen(true)).then((u) => uns.push(u));
    return () => uns.forEach((u) => u());
  }, []);

  const run = (fn: () => unknown | Promise<unknown>) => {
    Promise.resolve(fn()).catch((e) => toastError(e));
  };

  const startStack = async () => {
    if (!startReady || busy || quick.busy) return;
    await quick.start(selectedStack);
  };

  /** 真正执行「全部停止」，确认弹窗点确认后调用 */
  const doStopAll = async () => {
    if (busy) return;
    const report = await quick.stop(quick.stopReport?.failed.map((failure) => failure.serviceId));
    if (report && report.failed.length === 0) setConfirm(null);
  };

  const checkUpdate = async () => {
    // 统一走更新弹窗（含在线下载与安装），不再只弹一个 toast
    setUpdateOpen(true);
  };

  return (
    <>
      <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className={cn(
              "nsb-no-drag group relative z-10 flex min-w-0 items-center gap-2 rounded-lg px-1 py-1 text-left outline-none",
              "hover:bg-fill focus-visible:ring-2 focus-visible:ring-primary/30",
              collapsed && "justify-center px-0"
            )}
            aria-label={t("appmenu.label")}
          >
            <AppLogo />
            {!collapsed && (
              <>
                <span className="min-w-0 flex-1 truncate text-[13.5px] font-semibold tracking-tight">
                  NiceEnv
                </span>
                <ChevronDown className="h-3.5 w-3.5 shrink-0 text-faint transition-transform group-data-[state=open]:rotate-180" />
              </>
            )}
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-56" onCloseAutoFocus={(e) => e.preventDefault()}>
          <DropdownMenuLabel>
            NiceEnv <span className="font-mono text-faint">v{version}</span>
          </DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled={checking} onSelect={() => run(checkUpdate)}>
            <RefreshCw className={cn(checking && "animate-spin")} />
            {checking ? t("settings.checking") : t("appmenu.checkUpdate")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => run(() => api.openInBrowser(REPO_URL))}>
            <Github /> {t("appmenu.repo")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => run(() => api.openInBrowser(RELEASES_URL))}>
            <ExternalLink /> {t("appmenu.releases")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => setWizardOpen(true)}>
            <Plus /> {t("appmenu.newSite")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={busy || quick.busy || !startReady} onSelect={() => run(startStack)}>
            <Rocket /><span className="min-w-0 [overflow-wrap:anywhere]">{selectedStack ? `${t("dash.startStack")}「${selectedStack.name}」` : t("appmenu.startStack")}</span>
          </DropdownMenuItem>
          <DropdownMenuItem disabled={busy || quick.busy || !servicesReady || !quick.hasStopTargets} onSelect={async () => { if (await quick.prepareStop()) setConfirm("stopAll"); }}>
            <Square /> {t("appmenu.stopAll")}
          </DropdownMenuItem>
          {(serviceQuery.error || stackQuery.error) && <>
            <DropdownMenuLabel role="status" className="text-error [overflow-wrap:anywhere]">{t("stack.readFailed")}</DropdownMenuLabel>
            <DropdownMenuItem disabled={serviceQuery.isFetching || stackQuery.isFetching} onSelect={() => { void Promise.all([serviceQuery.refetch(), stackQuery.refetch()]); }}>
              <RefreshCw />{t("packages.reload")}
            </DropdownMenuItem>
          </>}
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => router.push("/settings")}>
            <Settings /> {t("appmenu.settings")}
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={() =>
              run(async () => {
                const dir = await api.getDataDir();
                await api.openInFolder(dir || ".");
              })
            }
          >
            <FolderOpen /> {t("appmenu.dataDir")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setAboutOpen(true)}>
            <Info /> {t("appmenu.about")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          {!isMac && (
            <DropdownMenuItem onSelect={() => minimize()}>
              <EyeOff /> {t("titlebar.min")}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onSelect={() => close()}>
            <EyeOff /> {t("appmenu.hide")}
          </DropdownMenuItem>
          <DropdownMenuItem
            className="text-error focus:text-error"
            disabled={busy || quick.busy}
            onSelect={() => {
              // 退出会停掉所有服务：先确认，避免误点导致站点全下线
              if (isTauri) { setQuitError(null); setConfirm("quit"); }
              else close();
            }}
          >
            <LogOut /> {t("appmenu.quit")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <UpdateDialog open={updateOpen} onOpenChange={setUpdateOpen} />

      <ConfirmDialog
        open={confirm === "stopAll"}
        onOpenChange={(o) => { if (!o && !quick.busy) setConfirm(null); }}
        title={t("confirm.stopAll")}
        description={quick.stopDescription}
        confirmText={t(quick.stopReport?.failed.length ? "bulk.retryFailed" : "dash.stopAll")}
        danger
        loading={quick.busy}
        onConfirm={doStopAll}
      >
        <BulkResult report={quick.stopReport} error={quick.stopError} services={services} busy={quick.busy} />
      </ConfirmDialog>

      <ConfirmDialog
        open={confirm === "quit"}
        onOpenChange={(o) => !busy && !o && setConfirm(null)}
        title={t("confirm.quit")}
        description={t("confirm.quitDesc")}
        confirmText={t("appmenu.quit")}
        danger
        loading={busy}
        onConfirm={async () => {
          if (quitBusy.current) return;
          quitBusy.current = true;
          setQuitError(null);
          setBusy(true);
          try {
            await api.quitApp();
            setConfirm(null);
          } catch (e) {
            const error = normalizeError(e);
            setQuitError([error.message, error.hint].filter(Boolean).join("\n"));
          } finally {
            quitBusy.current = false;
            setBusy(false);
          }
        }}
      >
        {busy && <p role="status" className="text-xs leading-relaxed text-muted">{t("confirm.quittingHint")}</p>}
        {quitError && <div ref={quitErrorRef} tabIndex={-1} role="alert" className="rounded-lg border border-error/30 bg-error-soft p-3 text-xs text-error whitespace-pre-wrap [overflow-wrap:anywhere]">{quitError}</div>}
      </ConfirmDialog>

      <Dialog open={aboutOpen} onOpenChange={setAboutOpen}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <div className="mb-2">
              <AppLogo className="h-9 w-9 rounded-xl" />
            </div>
            <DialogTitle>NiceEnv</DialogTitle>
            <DialogDescription>{t("about.desc")}</DialogDescription>
          </DialogHeader>
          <p className="text-[13px] text-secondary">
            {t("settings.currentVersion")}{" "}
            <code className="rounded bg-card-2 px-1.5 py-0.5 font-mono text-[12px]">v{version}</code>
          </p>
          <DialogFooter>
            <Button variant="secondary" onClick={() => api.openInBrowser(REPO_URL).catch(toastError)}>
              <Github className="h-3.5 w-3.5" /> GitHub
            </Button>
            <Button onClick={() => setAboutOpen(false)}>{t("common.close")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
