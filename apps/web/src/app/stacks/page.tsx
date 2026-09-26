"use client";

import * as React from "react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  Square,
  Plus,
  Copy,
  Trash2,
  Pencil,
  Layers,
  Loader2,
  ChevronUp,
  ChevronDown,
  X,
  Rocket,
  AlertTriangle,
  CheckCircle2,
} from "lucide-react";
import type { Stack, StackItem, PackageView, ServiceStatus } from "@nsb/schema";
import { cn, cmpVersionDesc, resolveStackService, resolvedStackItems } from "@/lib/utils";
import { useT } from "@/lib/store";
import { useInvalidate, useServices, useStacks, usePackages, toastError, toastPortConflict, toastStackReport, serviceHasProcess } from "@/lib/hooks";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
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
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export default function StacksPage() {
  const t = useT();
  const stackQuery = useStacks();
  const serviceQuery = useServices(3000);
  const packageQuery = usePackages();
  const { data: stacks, refetch } = stackQuery;
  const { data: services } = serviceQuery;
  const { data: packages } = packageQuery;
  const stateReady = serviceQuery.dataUpdatedAt > 0 && packageQuery.dataUpdatedAt > 0 && !serviceQuery.error && !packageQuery.error;
  const loadError = stackQuery.error || serviceQuery.error || packageQuery.error;
  const retryLoad = () => { void stackQuery.refetch(); void serviceQuery.refetch(); void packageQuery.refetch(); };
  const invalidate = useInvalidate();
  const [editing, setEditing] = React.useState<Stack | null>(null);
  const [creating, setCreating] = React.useState(false);
  const [removing, setRemoving] = React.useState<Stack | null>(null);
  const [busyId, setBusyId] = React.useState<string | null>(null);
  const busyRef = React.useRef(false);
  const [deleteError, setDeleteError] = React.useState<string | null>(null);

  const stateOf = (id: string) => stateReady ? resolveStackService(id, services, packages) : undefined;

  /** 一键启动：逐项结果回报，失败项单独提示（带「结束占用并重试」） */
  const startStack = async (stack: Stack) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusyId(stack.id);
    try {
      const report = await api.startStack(stack.id);
      toastStackReport(t, report, "start", () => startStack(stack));
    } catch (e) {
      // 栈本身起不来（没装/为空）或首个端口冲突
      if (!toastPortConflict(e, { onResolved: () => startStack(stack) })) toastError(e);
    } finally {
      busyRef.current = false;
      setBusyId(null);
      invalidate("services", "stacks");
    }
  };

  const stopStack = async (stack: Stack) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusyId(stack.id);
    try {
      const report = await api.stopStack(stack.id);
      toastStackReport(t, report, "stop", () => stopStack(stack));
    } catch (e) {
      toastError(e);
    } finally {
      busyRef.current = false;
      setBusyId(null);
      invalidate("services", "stacks");
    }
  };

  return (
    <div className="pb-8">
      <PageHeader
        title={t("stack.title")}
        subtitle={t("stack.subtitle")}
        actions={
          <Button disabled={busyId !== null} onClick={() => setCreating(true)}>
            <Plus className="h-3.5 w-3.5" /> {t("stack.new")}
          </Button>
        }
      />

      {loadError && (
        <div role="alert" className="mb-4 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-error/30 bg-error-soft p-4">
          <p className="text-sm text-error">{t("stack.readFailed")}</p>
          <Button size="sm" variant="secondary" onClick={retryLoad}>{t("bulk.retry")}</Button>
        </div>
      )}
      {stackQuery.dataUpdatedAt === 0 && stackQuery.isFetching ? (
        <p role="status" className="py-8 text-center text-sm text-muted">{t("common.loading")}</p>
      ) : stacks.length === 0 && !loadError ? (
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
              const resolved = resolvedStackItems(stack.items, services, packages);
              const running = resolved.filter(({ service }) => service?.state === "running").length;
              const total = resolved.length;
              const allRunning = stateReady && total > 0 && running === total;
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
                      <div className="min-w-0 flex-1">
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
                        {stateReady ? `${running}/${total}` : "—/—"}
                      </span>
                    </div>

                    {/* 服务清单 + 逐个状态 */}
                    <div className="flex flex-wrap gap-1.5">
                      {stack.items.map((item) => {
                        const svc = stateOf(item.serviceId);
                        return (
                          <span
                            key={item.serviceId}
                            className="flex max-w-full min-w-0 items-start gap-1.5 rounded-md bg-fill px-2 py-1 text-[11px]"
                            title={svc ? `${svc.label} · ${t(`state.${svc.state}`)}` : t(stateReady ? "stack.notInstalled" : "stack.statusPending")}
                          >
                            <StatusLight className="mt-1.5 shrink-0" state={svc?.state ?? "unknown"} size={5} />
                            <span className="min-w-0 [overflow-wrap:anywhere]">
                              <span className={cn(!svc && stateReady && "text-faint")}>{item.label || serviceFamilyName(item.serviceId.split("@")[0], packages, services)}</span>
                              <span className="block text-[10px] text-muted">{stackItemHint(item.serviceId, svc, packages, services, t, stateReady)}</span>
                            </span>
                          </span>
                        );
                      })}
                    </div>

                    <div className="mt-auto flex flex-wrap items-center gap-2 pt-1">
                      <Button
                        size="sm"
                        className="flex-1 basis-full sm:basis-auto"
                        disabled={busyId !== null || !stateReady}
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
                        disabled={busyId !== null || !stack.items.some((item) => { const service = stateOf(item.serviceId); return service && serviceHasProcess(service); })}
                        onClick={() => stopStack(stack)}
                      >
                        <Square className="h-3.5 w-3.5" /> {t("stack.stopAll")}
                      </Button>
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        title={t("stack.edit")}
                        disabled={busyId !== null}
                        onClick={() => setEditing(stack)}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        title={t("stack.duplicate")}
                        disabled={busyId !== null}
                        onClick={async () => {
                          if (busyRef.current) return;
                          busyRef.current = true; setBusyId(stack.id);
                          try {
                            await api.duplicateStack(stack.id);
                            toast.success(t("stack.duplicated"));
                            refetch();
                          } catch (e) {
                            toastError(e);
                          } finally { busyRef.current = false; setBusyId(null); }
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
                          disabled={busyId !== null}
                          onClick={() => { setDeleteError(null); setRemoving(stack); }}
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
        services={services}
        packages={packages}
        ready={stateReady}
        loadError={!!loadError}
        retryLoad={retryLoad}
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
        onOpenChange={(open) => { if (!open && !busyRef.current) setRemoving(null); }}
        title={`${t("common.delete")} ${removing?.name ?? ""}?`}
        description={t("stack.deleteHint")}
        danger
        confirmText={t("common.delete")}
        loading={busyId !== null}
        onConfirm={async () => {
          if (!removing || busyRef.current) return;
          busyRef.current = true; setBusyId(removing.id); setDeleteError(null);
          try {
            await api.deleteStack(removing.id);
            toast.success(t("stack.deleted"));
            setRemoving(null);
            refetch();
          } catch (e) {
            setDeleteError(normalizeError(e).message);
          } finally { busyRef.current = false; setBusyId(null); }
        }}
      >
        {deleteError && <p role="alert" className="text-sm text-error [overflow-wrap:anywhere]">{deleteError}</p>}
      </ConfirmDialog>
    </div>
  );
}

function serviceFamilyName(base: string, packages: PackageView[], services: ServiceStatus[]) {
  if (base === "php") return "PHP";
  if (base === "mysql") return "MySQL";
  return packages.find((p) => p.id === base)?.displayName ?? services.find((s) => s.id === base)?.label ?? base;
}

function stackItemHint(id: string, service: ServiceStatus | undefined, packages: PackageView[], services: ServiceStatus[], t: ReturnType<typeof useT>, ready = true) {
  if (!ready) return t("stack.statusPending");
  const base = id.split("@")[0];
  const pinned = id.includes("@");
  const multi = pinned || services.some((s) => s.id.startsWith(`${base}@`)) || packages.some((p) => p.id === base && p.run?.singleInstance === false);
  const mode = pinned ? t("stack.fixedVersion") : multi ? t("stack.followVersion") : t("stack.currentVersion");
  const version = pinned ? id.slice(base.length + 1) : service?.version;
  return `${mode}${version ? ` · ${version}` : ""}${!service ? ` · ${t("stack.notInstalled")}` : ""}`;
}

/* ============ 编辑器 ============ */
type EditableStackItem = StackItem & { editorKey: string };

function StackEditor({ open, stack, services, packages, ready, loadError, retryLoad, onOpenChange, onSaved }: {
  open: boolean;
  stack: Stack | null;
  services: ServiceStatus[];
  packages: PackageView[];
  ready: boolean;
  loadError: boolean;
  retryLoad: () => void;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}) {
  const t = useT();
  const formId = React.useId();
  const [name, setName] = React.useState("");
  const [description, setDescription] = React.useState("");
  const [items, setItems] = React.useState<EditableStackItem[]>([]);
  const itemSequence = React.useRef(0);
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const [error, setError] = React.useState<string | null>(null);
  const initialT = React.useRef(t);
  initialT.current = t;

  React.useEffect(() => {
    if (!open) return;
    setName(stack ? (stack.builtin ? `${stack.name} ${initialT.current("stack.copySuffix")}` : stack.name) : "");
    setDescription(stack?.description ?? "");
    setItems(stack ? [...stack.items].sort((a, b) => a.order - b.order).map((item, index) => ({ ...item, editorKey: `initial-${index}` })) : []);
    setError(null);
  }, [open, stack]);

  const groups = React.useMemo(() => [...new Set(services.map((s) => s.id.split("@")[0]))].sort().map((base) => {
    const versions = services.filter((s) => s.id.startsWith(`${base}@`)).sort((a, b) => cmpVersionDesc(a.version ?? "", b.version ?? ""));
    return { base, versions, label: serviceFamilyName(base, packages, services) };
  }), [services, packages]);
  const runtimeOnly = [...new Set(packages.filter((p) => p.install && !p.run && !services.some((s) => s.id.split("@")[0] === p.id)).map((p) => p.displayName))];
  const requestClose = (next: boolean) => { if (!busyRef.current) onOpenChange(next); };
  const move = (index: number, direction: -1 | 1) => {
    if (busyRef.current) return;
    setItems((current) => {
      const other = index + direction;
      if (other < 0 || other >= current.length) return current;
      const next = [...current];
      [next[index], next[other]] = [next[other], next[index]];
      return next.map((item, i) => ({ ...item, order: (i + 1) * 10 }));
    });
  };
  const save = async () => {
    if (busyRef.current) return;
    if (!name.trim()) { setError(t("stack.nameRequired")); return; }
    if (!items.length) { setError(t("stack.itemsRequired")); return; }
    busyRef.current = true; setBusy(true); setError(null);
    try {
      await api.saveStack({ id: stack && !stack.builtin ? stack.id : undefined,
        name: name.trim(), description: description.trim(),
        items: items.map((item, index) => ({ serviceId: item.serviceId, label: item.label, order: (index + 1) * 10 })),
      });
      toast.success(t("stack.saved"));
      onOpenChange(false); onSaved();
    } catch (e) { setError(normalizeError(e).message); }
    finally { busyRef.current = false; setBusy(false); }
  };

  return (
    <Dialog open={open} onOpenChange={requestClose}>
      {/* 保留淡入淡出；缩放动画会让版本下拉反复测量变化中的触发器宽度。 */}
      <DialogContent hideClose={busy} aria-busy={busy} className="flex max-h-[85dvh] max-w-2xl flex-col gap-0 overflow-hidden p-0 data-[state=open]:zoom-in-100 data-[state=closed]:zoom-out-100">
        <DialogHeader className="shrink-0 border-b border-border px-4 py-4 pr-12 sm:px-6 sm:pr-12">
          <DialogTitle>{stack ? t("stack.edit") : t("stack.new")}</DialogTitle>
          <DialogDescription>{stack?.builtin ? t("stack.presetSaveAs") : t("stack.editorHint")}</DialogDescription>
        </DialogHeader>
        <form id={formId} className="min-h-0 min-w-0 flex-1 overflow-y-auto px-4 py-4 sm:px-6" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <fieldset disabled={busy} className="min-w-0 space-y-4">
            <div className="space-y-1.5">
              <label htmlFor={`${formId}-name`} className="text-xs font-medium">{t("stack.name")}</label>
              <Input id={`${formId}-name`} value={name} onChange={(event) => setName(event.target.value)} autoComplete="off" aria-required="true" />
            </div>
            <div className="space-y-1.5">
              <label htmlFor={`${formId}-description`} className="text-xs font-medium">{t("stack.description")}</label>
              <Input id={`${formId}-description`} value={description} onChange={(event) => setDescription(event.target.value)} placeholder={t("stack.descriptionPlaceholder")} />
            </div>
            {loadError && <div role="alert" className="rounded-lg bg-error-soft p-3 text-xs text-error">{t("stack.readFailed")}<Button type="button" size="sm" variant="ghost" onClick={retryLoad}>{t("bulk.retry")}</Button></div>}
            <div className="space-y-2">
              <h3 className="text-xs font-medium">{t("stack.services")} ({items.length})</h3>
              {!items.length && <p className="rounded-lg border border-dashed border-border p-4 text-center text-xs text-faint">{t("stack.noItems")}</p>}
              <ol className="space-y-2">
                {items.map((item, index) => {
                  const base = item.serviceId.split("@")[0];
                  const group = groups.find((g) => g.base === base);
                  const service = resolveStackService(item.serviceId, services, packages);
                  const label = item.label || serviceFamilyName(base, packages, services);
                  const multi = !!group?.versions.length || item.serviceId.includes("@");
                  const selectedMissing = item.serviceId.includes("@") && !services.some((s) => s.id === item.serviceId);
                  return (
                    <li key={item.editorKey} className="min-w-0 rounded-xl border border-border bg-fill/40 p-3">
                      <div className="flex min-w-0 items-center gap-2">
                        <span className="shrink-0 text-xs tabular text-faint">{index + 1}.</span>
                        <span className="min-w-0 flex-1 truncate text-xs font-medium">{label}</span>
                        <div className="flex shrink-0 gap-0.5">
                          <Button type="button" size="icon-sm" variant="ghost" title={`${t("stack.moveUp")} ${label}`} onClick={() => move(index, -1)} disabled={busy || index === 0}><ChevronUp className="h-4 w-4" /></Button>
                          <Button type="button" size="icon-sm" variant="ghost" title={`${t("stack.moveDown")} ${label}`} onClick={() => move(index, 1)} disabled={busy || index === items.length - 1}><ChevronDown className="h-4 w-4" /></Button>
                          <Button type="button" size="icon-sm" variant="ghost" title={`${t("stack.removeItem")} ${label}`} onClick={() => setItems((current) => current.filter((_, i) => i !== index))} disabled={busy}><X className="h-3.5 w-3.5" /></Button>
                        </div>
                      </div>
                      {multi && (
                        <Select value={item.serviceId} disabled={busy || !ready} onValueChange={(serviceId) => setItems((current) => current.some((other, i) => i !== index && other.serviceId === serviceId) ? current : current.map((other, i) => i === index ? { ...other, serviceId } : other))}>
                          <SelectTrigger className="mt-2 min-w-0 text-xs" aria-label={`${label} ${t("stack.versionRule")}`}><SelectValue /></SelectTrigger>
                          <SelectContent className="max-w-[calc(100vw-2rem)]">
                            <SelectItem value={base} disabled={items.some((other, i) => i !== index && other.serviceId === base)}>{t("stack.followVersion")}</SelectItem>
                            <SelectSeparator />
                            {group?.versions.map((version) => <SelectItem key={version.id} value={version.id} disabled={items.some((other, i) => i !== index && other.serviceId === version.id)}>{t("stack.fixedVersion")} {version.version}</SelectItem>)}
                            {selectedMissing && <SelectItem value={item.serviceId}>{t("stack.fixedVersion")} {item.serviceId.slice(base.length + 1)} · {t("stack.notInstalled")}</SelectItem>}
                          </SelectContent>
                        </Select>
                      )}
                      <p className={cn("mt-2 text-[11px] [overflow-wrap:anywhere]", ready && !service ? "text-warn" : "text-muted")}>{stackItemHint(item.serviceId, service, packages, services, t, ready)}</p>
                    </li>
                  );
                })}
              </ol>
              <p className="text-[11px] text-faint">{t("stack.orderHint")}</p>
              <p className="text-[11px] text-muted">{t("stack.versionHint")}</p>
            </div>
            <div className="space-y-2">
              <h3 className="text-xs font-medium">{t("stack.addService")}</h3>
              {!ready ? <p className="text-xs text-muted">{t(loadError ? "stack.statusPending" : "common.loading")}</p> : !groups.length ? <p className="text-xs text-faint">{t("stack.noCandidates")}</p> : (
                <div className="flex flex-wrap gap-2">
                  {groups.map((group) => {
                    const candidate = [group.base, ...group.versions.map((v) => v.id)].find((id) => !items.some((item) => item.serviceId === id));
                    return <Button key={group.base} type="button" size="sm" variant="secondary" disabled={busy || !candidate} onClick={() => {
                      if (!candidate) return;
                      const editorKey = `added-${itemSequence.current++}`;
                      setItems((current) => current.some((item) => item.serviceId === candidate) ? current : [...current, { serviceId: candidate, order: (current.length + 1) * 10, editorKey }]);
                    }}><Plus className="h-3.5 w-3.5" />{group.label}</Button>;
                  })}
                </div>
              )}
              {!!runtimeOnly.length && <p className="text-[11px] text-faint [overflow-wrap:anywhere]">{t("stack.runtimeOnly")} {runtimeOnly.join(", ")}</p>}
            </div>
          </fieldset>
        </form>
        {error && <p role="alert" className="mx-4 mb-3 max-h-24 shrink-0 overflow-y-auto rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere] sm:mx-6">{error}</p>}
        <DialogFooter className="shrink-0 border-t border-border px-4 py-3 sm:px-6">
          <Button type="button" variant="ghost" onClick={() => requestClose(false)} disabled={busy}>{t("common.cancel")}</Button>
          <Button form={formId} type="submit" disabled={busy}>
            {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <CheckCircle2 className="h-3.5 w-3.5" />}{t("common.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
