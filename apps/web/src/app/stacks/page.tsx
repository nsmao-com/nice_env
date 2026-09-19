"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  Play,
  Square,
  Plus,
  Copy,
  Trash2,
  Pencil,
  Layers,
  Loader2,
  GripVertical,
  Rocket,
  AlertTriangle,
  CheckCircle2,
} from "lucide-react";
import type { Stack, StackItem, StackStartReport } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { useInvalidate, useServices, useStacks, toastError, toastPortConflict } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { EmptyState, ConfirmDialog } from "@/components/shared/misc";
import { StatusLight } from "@/components/shared/status-light";
import { PageHeader } from "@/components/layout/app-shell";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export default function StacksPage() {
  const t = useT();
  const { data: stacks, refetch } = useStacks();
  const { data: services } = useServices(3000);
  const invalidate = useInvalidate();
  const [editing, setEditing] = React.useState<Stack | null>(null);
  const [creating, setCreating] = React.useState(false);
  const [removing, setRemoving] = React.useState<Stack | null>(null);
  const [busyId, setBusyId] = React.useState<string | null>(null);

  /** 每个服务当前状态（栈卡片上的小圆点） */
  const stateOf = React.useCallback(
    (serviceId: string) => {
      const direct = services.find((s) => s.id === serviceId);
      if (direct) return direct;
      // 栈里写的是无版本 id（php / mysql）→ 跟随「使用中版本」
      const base = serviceId.split("@")[0];
      return services.find((s) => s.id.startsWith(`${base}@`));
    },
    [services]
  );

  const runningOf = (stack: Stack) =>
    stack.items.filter((i) => stateOf(i.serviceId)?.state === "running").length;

  /** 一键启动：逐项结果回报，失败项单独提示（带「结束占用并重试」） */
  const startStack = async (stack: Stack) => {
    setBusyId(stack.id);
    try {
      const report = await api.startStack(stack.id);
      reportToast(t, report, t("stack.startedOk"));
      invalidate("services", "stacks");
    } catch (e) {
      // 栈本身起不来（没装/为空）或首个端口冲突
      if (!toastPortConflict(e, { onResolved: () => startStack(stack) })) toastError(e);
    } finally {
      setBusyId(null);
    }
  };

  const stopStack = async (stack: Stack) => {
    setBusyId(stack.id);
    try {
      const report = await api.stopStack(stack.id);
      reportToast(t, report, t("stack.stoppedOk"));
      invalidate("services", "stacks");
    } catch (e) {
      toastError(e);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="pb-8">
      <PageHeader
        title={t("stack.title")}
        subtitle={t("stack.subtitle")}
        actions={
          <Button onClick={() => setCreating(true)}>
            <Plus className="h-3.5 w-3.5" /> {t("stack.new")}
          </Button>
        }
      />

      {stacks.length === 0 ? (
        <EmptyState
          icon={Layers}
          title={t("stack.empty")}
          hint={t("stack.emptyHint")}
          action={
            <Button onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" /> {t("stack.new")}
            </Button>
          }
        />
      ) : (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2 2xl:grid-cols-3">
          <AnimatePresence>
            {stacks.map((stack) => {
              const running = runningOf(stack);
              const total = stack.items.length;
              const allRunning = total > 0 && running === total;
              return (
                <motion.div
                  key={stack.id}
                  layout
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, scale: 0.98 }}
                >
                  <Card className={cn("flex h-full flex-col gap-3 p-4", allRunning && "border-running/25")}>
                    <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="truncate text-[14px] font-medium">{stack.name}</span>
                          {stack.builtin && <Badge variant="default">{t("stack.preset")}</Badge>}
                        </div>
                        <p className="mt-0.5 line-clamp-2 text-[11.5px] text-faint">
                          {stack.description || t("stack.noDescription")}
                        </p>
                      </div>
                      <span
                        className={cn(
                          "shrink-0 rounded-full px-2 py-0.5 text-[10.5px] tabular",
                          allRunning
                            ? "bg-running-soft text-running"
                            : running > 0
                              ? "bg-warn/10 text-warn"
                              : "bg-card-2 text-faint"
                        )}
                      >
                        {running}/{total}
                      </span>
                    </div>

                    {/* 服务清单 + 逐个状态 */}
                    <div className="flex flex-wrap gap-1.5">
                      {stack.items.map((item) => {
                        const svc = stateOf(item.serviceId);
                        return (
                          <span
                            key={item.serviceId}
                            className="flex items-center gap-1.5 rounded-md border border-border bg-card-2/40 px-2 py-1 text-[11px]"
                            title={svc ? `${svc.label} · ${svc.state}` : t("stack.notInstalled")}
                          >
                            <StatusLight state={svc?.state ?? "unknown"} size={5} />
                            <span className={cn("font-mono", !svc && "text-faint line-through")}>
                              {item.label || item.serviceId}
                            </span>
                          </span>
                        );
                      })}
                    </div>

                    <div className="mt-auto flex items-center gap-2 pt-1">
                      <Button
                        size="sm"
                        className="flex-1"
                        disabled={busyId === stack.id}
                        onClick={() => startStack(stack)}
                      >
                        {busyId === stack.id ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <Rocket className="h-3.5 w-3.5" />
                        )}
                        {t("stack.startAll")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busyId === stack.id || running === 0}
                        onClick={() => stopStack(stack)}
                      >
                        <Square className="h-3.5 w-3.5" /> {t("stack.stopAll")}
                      </Button>
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        title={t("stack.edit")}
                        onClick={() => setEditing(stack)}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        title={t("stack.duplicate")}
                        onClick={async () => {
                          try {
                            await api.duplicateStack(stack.id);
                            toast.success(t("stack.duplicated"));
                            refetch();
                          } catch (e) {
                            toastError(e);
                          }
                        }}
                      >
                        <Copy className="h-3.5 w-3.5" />
                      </Button>
                      {!stack.builtin && (
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          className="text-error/80 hover:text-error"
                          title={t("common.delete")}
                          onClick={() => setRemoving(stack)}
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </Button>
                      )}
                    </div>
                  </Card>
                </motion.div>
              );
            })}
          </AnimatePresence>
        </div>
      )}

      <StackEditor
        open={creating || editing !== null}
        stack={editing}
        onOpenChange={(open) => {
          if (!open) {
            setCreating(false);
            setEditing(null);
          }
        }}
        onSaved={() => {
          refetch();
          invalidate("services");
        }}
      />

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={`${t("common.delete")} ${removing?.name ?? ""}?`}
        description={t("stack.deleteHint")}
        danger
        confirmText={t("common.delete")}
        onConfirm={async () => {
          if (!removing) return;
          try {
            await api.deleteStack(removing.id);
            toast.success(t("stack.deleted"));
            setRemoving(null);
            refetch();
          } catch (e) {
            toastError(e);
          }
        }}
      />
    </div>
  );
}

/** 把逐项报告转成一条人话 toast */
function reportToast(
  t: ReturnType<typeof import("@/lib/store").useT>,
  report: StackStartReport,
  okTitle: string
) {
  const okCount = report.started.length;
  const already = report.alreadyRunning.length;
  const failed = report.failed;
  if (failed.length === 0) {
    toast.success(okTitle, {
      description: [
        okCount > 0 ? `${okCount} ${t("stack.rptStarted")}` : "",
        already > 0 ? `${already} ${t("stack.rptAlready")}` : "",
      ]
        .filter(Boolean)
        .join(" · "),
    });
    return;
  }
  // 有失败项：优先显示第一条端口冲突（附「结束占用并重试」）
  const portFail = failed.find((f) => f.error.code === "PORT_IN_USE");
  if (portFail) {
    const e = { ...portFail.error } as { code: string; message: string; hint?: string; port?: number };
    if (toastPortConflict(e, {})) {
      const rest = failed.filter((f) => f !== portFail);
      if (rest.length > 0) {
        toast.warning(
          `${rest.length} ${t("stack.rptFailed")}`,
          { description: rest.map((f) => `${f.serviceId}: ${f.error.message}`).join("\n"), duration: 10000 }
        );
      }
      return;
    }
  }
  toast.warning(`${failed.length} ${t("stack.rptFailed")}`, {
    description: failed.map((f) => `${f.serviceId}: ${f.error.message}`).join("\n"),
    duration: 10000,
  });
}

/* ============ 编辑器 ============ */

function StackEditor({
  open,
  stack,
  onOpenChange,
  onSaved,
}: {
  open: boolean;
  stack: Stack | null;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}) {
  const t = useT();
  const { data: services } = useServices(0);
  const { data: packages } = usePackagesSafe();
  const [name, setName] = React.useState("");
  const [description, setDescription] = React.useState("");
  const [items, setItems] = React.useState<StackItem[]>([]);
  const [busy, setBusy] = React.useState(false);

  React.useEffect(() => {
    if (!open) return;
    // 内置预设不可直接改：打开时引导用户「另存为」（保存时由后端拦住，这里给出更早的提示）
    setName(stack ? (stack.builtin ? `${stack.name} 副本` : stack.name) : "");
    setDescription(stack?.description ?? "");
    setItems(stack ? stack.items.map((i) => ({ ...i })) : []);
  }, [open, stack]);

  /** 可选服务：已注册的（可启停的）服务，按 id 排序 */
  const candidates = React.useMemo(() => {
    const list = services.map((s) => ({ id: s.id, label: s.label, state: s.state }));
    return list.sort((a, b) => a.id.localeCompare(b.id));
  }, [services]);

  /** 已安装但未注册为服务的包（纯运行时）——给出解释，避免用户以为漏了 */
  const runtimeOnly = React.useMemo(
    () => (packages ?? []).filter((p) => p.install && !p.run).map((p) => p.displayName),
    [packages]
  );

  const addItem = (serviceId: string) => {
    if (items.some((i) => i.serviceId === serviceId)) return;
    setItems((prev) => [...prev, { serviceId, order: (prev.length + 1) * 10 }]);
  };

  const move = (idx: number, dir: -1 | 1) => {
    const next = idx + dir;
    if (next < 0 || next >= items.length) return;
    setItems((prev) => {
      const copy = [...prev];
      [copy[idx], copy[next]] = [copy[next], copy[idx]];
      return copy.map((it, i) => ({ ...it, order: (i + 1) * 10 }));
    });
  };

  const save = async () => {
    if (!name.trim()) {
      toast.error(t("stack.nameRequired"));
      return;
    }
    if (items.length === 0) {
      toast.error(t("stack.itemsRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.saveStack({
        // 内置预设：不带 id → 后端生成新栈（等于「另存为」）
        id: stack && !stack.builtin ? stack.id : undefined,
        name: name.trim(),
        description: description.trim(),
        items: items.map((it, i) => ({ ...it, order: (i + 1) * 10 })),
      });
      toast.success(t("stack.saved"));
      onOpenChange(false);
      onSaved();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{stack ? t("stack.edit") : t("stack.new")}</DialogTitle>
          <DialogDescription>
            {stack?.builtin ? t("stack.presetSaveAs") : t("stack.editorHint")}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <label className="text-[12px] font-medium text-secondary">{t("stack.name")}</label>
            <Input value={name} onChange={(e) => setName(e.target.value)} className="h-8 text-[12.5px]" />
          </div>
          <div className="flex flex-col gap-1.5">
            <label className="text-[12px] font-medium text-secondary">{t("stack.description")}</label>
            <Input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder={t("stack.descriptionPlaceholder")}
              className="h-8 text-[12.5px]"
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <label className="text-[12px] font-medium text-secondary">
              {t("stack.services")} ({items.length})
            </label>
            <div className="max-h-52 overflow-y-auto rounded-lg border border-border bg-card-2/30 p-1.5">
              {items.length === 0 ? (
                <p className="px-2 py-4 text-center text-[11.5px] text-faint">{t("stack.noItems")}</p>
              ) : (
                items.map((item, idx) => {
                  const svc = services.find((s) => s.id === item.serviceId);
                  return (
                    <div
                      key={item.serviceId}
                      className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-card-2/60"
                    >
                      <GripVertical className="h-3.5 w-3.5 shrink-0 text-faint/60" />
                      <span className="w-5 shrink-0 text-[10.5px] tabular text-faint">{idx + 1}</span>
                      <span className="min-w-0 flex-1 truncate text-[12px]">
                        {item.label || svc?.label || item.serviceId}
                        <span className="ml-1.5 font-mono text-[10.5px] text-faint">{item.serviceId}</span>
                      </span>
                      <div className="flex shrink-0 items-center gap-0.5">
                        <button
                          type="button"
                          onClick={() => move(idx, -1)}
                          disabled={idx === 0}
                          className="rounded px-1 text-[10px] text-faint hover:bg-card-2 hover:text-foreground disabled:opacity-30"
                        >
                          ↑
                        </button>
                        <button
                          type="button"
                          onClick={() => move(idx, 1)}
                          disabled={idx === items.length - 1}
                          className="rounded px-1 text-[10px] text-faint hover:bg-card-2 hover:text-foreground disabled:opacity-30"
                        >
                          ↓
                        </button>
                        <button
                          type="button"
                          onClick={() => setItems((prev) => prev.filter((i) => i.serviceId !== item.serviceId))}
                          className="rounded px-1 text-[11px] text-error/70 hover:text-error"
                        >
                          ✕
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>
            <p className="text-[10.5px] text-faint">{t("stack.orderHint")}</p>
          </div>

          <div className="flex flex-col gap-1.5">
            <label className="text-[12px] font-medium text-secondary">{t("stack.addService")}</label>
            {candidates.length === 0 ? (
              <p className="rounded-lg border border-dashed border-border px-3 py-3 text-[11.5px] text-faint">
                {t("stack.noCandidates")}
              </p>
            ) : (
              <div className="flex flex-wrap gap-1.5">
                {candidates.map((c) => {
                  const used = items.some((i) => i.serviceId === c.id);
                  return (
                    <button
                      key={c.id}
                      type="button"
                      disabled={used}
                      onClick={() => addItem(c.id)}
                      className={cn(
                        "flex items-center gap-1.5 rounded-md border px-2 py-1 text-[11.5px] transition-colors",
                        used
                          ? "border-border bg-card-2/20 text-faint/50"
                          : "border-border bg-card-2/50 text-secondary hover:border-border-strong hover:text-foreground"
                      )}
                    >
                      <StatusLight state={c.state} size={5} />
                      {c.label}
                    </button>
                  );
                })}
              </div>
            )}
            {runtimeOnly.length > 0 && (
              <p className="text-[10.5px] text-faint">
                {t("stack.runtimeOnly")} {runtimeOnly.join(", ")}
              </p>
            )}
          </div>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            {t("common.cancel")}
          </Button>
          <Button onClick={save} disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <CheckCircle2 className="h-3.5 w-3.5" />}
            {t("common.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 只读的包列表（编辑器的「纯运行时」提示用；失败不阻断） */
function usePackagesSafe() {
  const [data, setData] = React.useState<Awaited<ReturnType<typeof api.listPackages>>>([]);
  React.useEffect(() => {
    let alive = true;
    api
      .listPackages()
      .then((v) => alive && setData(v))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, []);
  return { data };
}
