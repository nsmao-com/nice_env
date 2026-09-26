"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";
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
import { useServices, toastError } from "@/lib/hooks";
import * as api from "@/lib/api";
import { isTauri, listen, normalizeError } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { ConfirmDialog } from "@/components/shared/misc";
import { UpdateDialog } from "@/components/shared/update-dialog";
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
  const { data: services } = useServices(0);
  const { isMac, minimize, close } = useDesktopWindow();
  const [aboutOpen, setAboutOpen] = React.useState(false);
  const [checking, setChecking] = React.useState(false);
  const [version, setVersion] = React.useState("0.2.14");
  const [updateOpen, setUpdateOpen] = React.useState(false);
  const [confirm, setConfirm] = React.useState<null | "stopAll" | "quit">(null);
  const [busy, setBusy] = React.useState(false);

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
    const ids = services
      .filter((s) => s.id === "nginx" || s.id === "redis" || s.id.startsWith("php@") || s.id.startsWith("mysql@"))
      .sort((a, b) => {
        const order = (id: string) =>
          id === "nginx" ? 0 : id.startsWith("php@") ? 1 : id.startsWith("mysql@") ? 2 : 3;
        return order(a.id) - order(b.id);
      })
      .map((s) => s.id);
    if (ids.length === 0) {
      toast.info(t("dashboard.noStackServices"));
      return;
    }
    toast.promise(
      (async () => {
        const failures: string[] = [];
        for (const id of ids) {
          try {
            await api.startService(id);
          } catch (error) {
            failures.push(id + ": " + normalizeError(error).message);
          }
        }
        if (failures.length > 0) {
          throw new Error(failures.join("；"));
        }
      })(),
      { loading: t("dashboard.startingStack"), success: t("dashboard.stackStarted"), error: t("cmd.startFail") }
    );
  };

  /** 真正执行「全部停止」，确认弹窗点确认后调用 */
  const doStopAll = async () => {
    setBusy(true);
    try {
      const failures: string[] = [];
      for (const s of services) {
        if (s.state === "running" || s.state === "starting") {
          try {
            await api.stopService(s.id);
          } catch (error) {
            failures.push(s.label + ": " + normalizeError(error).message);
          }
        }
      }
      if (failures.length > 0) {
        toast.error(t("dashboard.stopFailed"), { description: failures.join("；") });
      } else {
        toast.success(t("dashboard.allStopped"));
      }
      setConfirm(null);
    } finally {
      setBusy(false);
    }
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
          <DropdownMenuItem onSelect={() => run(startStack)}>
            <Rocket /> {t("appmenu.startStack")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setConfirm("stopAll")}>
            <Square /> {t("appmenu.stopAll")}
          </DropdownMenuItem>
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
            onSelect={() => {
              // 退出会停掉所有服务：先确认，避免误点导致站点全下线
              if (isTauri) setConfirm("quit");
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
        onOpenChange={(o) => !o && setConfirm(null)}
        title={t("confirm.stopAll")}
        description={t("confirm.stopAllDesc").replace(
          "{count}",
          String(services.filter((s) => s.state === "running" || s.state === "starting").length)
        )}
        confirmText={t("dash.stopAll")}
        danger
        loading={busy}
        onConfirm={doStopAll}
      />

      <ConfirmDialog
        open={confirm === "quit"}
        onOpenChange={(o) => !o && setConfirm(null)}
        title={t("confirm.quit")}
        description={t("confirm.quitDesc")}
        confirmText={t("appmenu.quit")}
        danger
        onConfirm={async () => {
          setBusy(true);
          try {
            await api.quitApp();
          } catch (e) {
            toastError(e);
          } finally {
            setBusy(false);
            setConfirm(null);
          }
        }}
      />

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
