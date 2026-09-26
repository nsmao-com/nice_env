"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  FilePenLine,
  Radar,
  SquareTerminal,
  FileCode2,
  DatabaseBackup,
  Wrench,
  Loader2,
  XCircle,
  Search,
  Power,
  Trash2,
  Stethoscope,
  RefreshCw,
} from "lucide-react";
import type { ListenerInfo, PortDiagnosis, PortScanEntry , HostsEntry } from "@nsb/schema";
import { HostsEntry as HostsEntrySchema } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useHosts, useInvalidate, toastError, useSettings, useSites, useServices } from "@/lib/hooks";
import * as api from "@/lib/api";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CopyButton, ConfirmDialog } from "@/components/shared/misc";
import { CodeBlock, useCodePalette } from "@/components/shared/code-block";
import { PathEnvCard } from "@/components/shared/path-env-card";
import { ConfigEditor } from "@/components/shared/config-editor";
import { DiagnosticsCard } from "@/components/shared/diagnostics-card";
import {
  CronTool,
  TunnelTool,
  OllamaTool,
  AdminerTool,
} from "@/components/shared/toolbox-tools";

const COMMON_PORTS = [80, 443, 8080, 8443, 3306, 23306, 6379, 26379, 9000, 5432];

export default function ToolsPage() {
  const t = useT();
  const pendingTool = useUI((st) => st.pendingTool);
  const consumeTool = useUI((st) => st.consumeTool);

  // 命令面板选「诊断报告 / 编辑配置」时，滚到对应卡片并让用户看到它
  React.useEffect(() => {
    if (!pendingTool) return;
    const id = pendingTool === "diagnostics" ? "nsb-tool-diagnostics" : "nsb-tool-config";
    // 等一帧，确保卡片已挂载
    const raf = window.requestAnimationFrame(() => {
      document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "center" });
    });
    consumeTool();
    return () => window.cancelAnimationFrame(raf);
  }, [pendingTool, consumeTool]);

  return (
    <div className="pb-8">
      <PageHeaderInline title={t("tools.title")} subtitle={t("tools.subtitle")} />
      <div className="grid grid-cols-1 gap-5 xl:grid-cols-2">
        <PortLookupTool />
        <DnsTool />
        <HostsTool />
        <PortTool />
        <PathEnvCard />
        <TerminalInjectTool />
        <RewriteTemplates />
        <div id="nsb-tool-config">
          <ConfigEditor />
        </div>
        <div id="nsb-tool-diagnostics">
          <ToolCard icon={Stethoscope} title={t("diag.title")} hint={t("diag.hint")}>
            <DiagnosticsCard />
          </ToolCard>
        </div>
        <BackupTool />
        <RepairTool />
        <CronTool />
        <TunnelTool />
        <OllamaTool />
        <AdminerTool />
      </div>
    </div>
  );
}

function PageHeaderInline({ title, subtitle }: { title: string; subtitle?: string }) {
  return (
    <div className="mb-6">
      <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
      {subtitle && <p className="mt-1 text-[13px] text-muted">{subtitle}</p>}
    </div>
  );
}

function ToolCard({
  icon: Icon,
  title,
  hint,
  action,
  children,
}: {
  icon: React.ComponentType<{ className?: string; strokeWidth?: number }>;
  title: string;
  hint: string;
  /** 头部右侧动作区（如 hosts 的导入/导出/模式切换） */
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <Card>
      <CardHeader className="flex-row items-start gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
          <Icon className="h-4 w-4 shrink-0 text-primary" strokeWidth={1.8} />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{title}</CardTitle>
          <CardDescription className="mt-0.5 line-clamp-2 text-[11px] leading-relaxed">{hint}</CardDescription>
        </div>
        {action && <div className="shrink-0">{action}</div>}
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

/* ============ hosts 编辑 ============ */

function HostsTool() {
  const t = useT();
  const paletteVars = useCodePalette();
  const hosts = useHosts();
  const siteQuery = useSites();
  const entries = hosts.data;
  const [newDomain, setNewDomain] = React.useState("");
  const [newIp, setNewIp] = React.useState("127.0.0.1");
  const [editing, setEditing] = React.useState<HostsEntry | null>(null);
  const [deleteTarget, setDeleteTarget] = React.useState<HostsEntry | null>(null);
  const [clearOpen, setClearOpen] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const action = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [mode, setMode] = React.useState<"list" | "text">("list");
  const [textDraft, setTextDraft] = React.useState("");
  const [textBaseline, setTextBaseline] = React.useState("");
  const textSnapshot = React.useRef<HostsEntry[]>([]);
  const formRef = React.useRef<HTMLDivElement>(null);
  const errorRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);
  const ready = hosts.dataUpdatedAt > 0 && siteQuery.dataUpdatedAt > 0 && !hosts.error && !siteQuery.error;
  const disabled = busy || !ready;
  const siteDomains = React.useMemo(
    () => new Set(siteQuery.data.flatMap((site) => site.domains.map((domain) => domain.toLowerCase().replace(/\.$/, "")))),
    [siteQuery.data]
  );
  const editable = (entry: HostsEntry) => entry.managed && !siteDomains.has(entry.domain);
  const manual = entries.filter(editable);
  const sameEntry = (a: HostsEntry, b: HostsEntry) => a.domain === b.domain && a.ip === b.ip && a.managed === b.managed;
  const reportError = (e: unknown) => { setError(normalizeError(e)); };

  const validate = (ip: string, domain: string): HostsEntry => {
    const result = HostsEntrySchema.safeParse({ ip, domain, managed: true });
    if (!result.success) throw { code: "HOSTS_INVALID", message: t("tools.hostsInvalid") };
    if (siteDomains.has(result.data.domain)) throw { code: "HOSTS_SITE_MANAGED", message: t("tools.hostsSiteHint") };
    return result.data;
  };
  const parseText = (text: string): HostsEntry[] => {
    const parsed: HostsEntry[] = [];
    const seen = new Set<string>();
    for (const [index, raw] of text.split(/\r?\n/).entries()) {
      const line = raw.split("#")[0].trim();
      if (!line) continue;
      try {
        const [ip, ...domains] = line.split(/\s+/);
        if (!domains.length) throw { code: "HOSTS_INVALID", message: t("tools.hostsInvalid") };
        for (const domain of domains) {
          const entry = validate(ip, domain);
          const key = `${entry.ip}|${entry.domain}`;
          if (!seen.has(key)) { seen.add(key); parsed.push(entry); }
        }
      } catch (e) {
        throw { ...normalizeError(e), message: `${t("tools.hostsLine")} ${index + 1}: ${normalizeError(e).message}` };
      }
    }
    return parsed;
  };
  const resetForm = () => { setEditing(null); setNewDomain(""); setNewIp("127.0.0.1"); };
  const run = async (operation: () => Promise<void>) => {
    if (action.current || !ready) return;
    action.current = true;
    setBusy(true);
    setError(null);
    try { await operation(); }
    catch (e) {
      const failure = normalizeError(e);
      reportError(failure.code === "HOSTS_CHANGED" && mode === "text" ? { ...failure, hint: t("tools.hostsTextChanged") } : failure);
    }
    finally { action.current = false; setBusy(false); }
  };
  const apply = async (next: HostsEntry[], expected = entries) => {
    await api.applyHosts(next, expected);
    const refreshed = await hosts.refetch();
    if (refreshed.error) throw { code: "HOSTS_REFRESH_FAILED", message: t("tools.hostsSavedReadFailed"), hint: normalizeError(refreshed.error).message };
  };
  const saveEntry = () => run(async () => {
    const entry = validate(newIp, newDomain);
    const others = manual.filter((value) => !editing || !sameEntry(value, editing));
    if (editing && !manual.some((value) => sameEntry(value, editing))) throw { code: "HOSTS_CHANGED", message: t("tools.hostsChanged") };
    if (others.some((value) => sameEntry(value, entry))) throw { code: "HOSTS_DUPLICATE", message: t("tools.hostsDuplicate") };
    await apply([...others, entry]);
    resetForm();
    toast.success(`${t("tools.hostsUpdatedP1")}${entry.domain}`);
  });
  const remove = () => run(async () => {
    if (!deleteTarget) return;
    await apply(manual.filter((entry) => !sameEntry(entry, deleteTarget)));
    if (editing && sameEntry(editing, deleteTarget)) resetForm();
    setDeleteTarget(null);
    toast.success(t("tools.hostsDeleted"));
  });
  const openText = () => {
    if (disabled) return;
    const content = manual.map((entry) => `${entry.ip} ${entry.domain}`).join("\n");
    setTextDraft(content);
    setTextBaseline(content);
    textSnapshot.current = entries;
    setError(null);
    setMode("text");
  };
  const applyText = (confirmed = false) => run(async () => {
    const parsed = parseText(textDraft);
    if (!parsed.length && manual.length && !confirmed) { setClearOpen(true); return; }
    await apply(parsed, textSnapshot.current);
    setClearOpen(false);
    setMode("list");
    resetForm();
    toast.success(`${t("tools.hostsUpdatedP1")}${parsed.length}`);
  });
  const importFile = () => run(async () => {
    if (!isTauri) { toast.info(t("tools.hostsDesktopOnly")); return; }
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ multiple: false, filters: [{ name: "hosts", extensions: ["hosts", "txt", "conf"] }] });
    if (!path || typeof path !== "string") return;
    const parsed = parseText(await api.readTextFile(path));
    if (!parsed.length) { toast.info(t("tools.hostsImportEmpty")); return; }
    const domains = new Set(parsed.map((entry) => entry.domain));
    await apply([...manual.filter((entry) => !domains.has(entry.domain)), ...parsed]);
    resetForm();
    toast.success(`${t("tools.hostsImportedP1")} ${parsed.length} ${t("tools.hostsImportedP2")}`);
  });
  const exportFile = () => run(async () => {
    const body = manual.map((entry) => `${entry.ip}\t${entry.domain}`).join("\n");
    if (!isTauri) {
      const url = URL.createObjectURL(new Blob([body], { type: "text/plain;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = "hosts-nsb.txt";
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    } else {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const path = await save({ defaultPath: "hosts-nsb.txt", filters: [{ name: "hosts", extensions: ["txt", "hosts"] }] });
      if (!path || typeof path !== "string") return;
      await api.writeTextFile(path, body);
    }
    toast.success(t("tools.hostsExported"));
  });
  const errorBox = error && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-1 rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">
    <p>{error.message}</p>{error.hint && <p>{error.hint}</p>}
  </div>;

  return (
    <ToolCard icon={FilePenLine} title={t("tools.hosts")} hint={t("tools.hostsHint")}>
      <div className="flex min-w-0 flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap gap-1">
            <Button size="sm" variant="ghost" disabled={disabled || mode === "text"} onClick={importFile}>{t("tools.hostsImport")}</Button>
            <Button size="sm" variant="ghost" disabled={disabled || mode === "text"} onClick={exportFile}>{t("tools.hostsExport")}</Button>
            <Button size="sm" variant="ghost" disabled={busy || hosts.isFetching || siteQuery.isFetching}
              onClick={() => { void hosts.refetch(); void siteQuery.refetch(); }}>{t("tools.refresh")}</Button>
          </div>
          <Tabs value={mode} onValueChange={(value) => { if (value === "text") openText(); else setMode("list"); }}>
            <TabsList>
              <TabsTrigger value="list" disabled={busy || (mode === "text" && textDraft !== textBaseline)}>{t("tools.hostsModeList")}</TabsTrigger>
              <TabsTrigger value="text" disabled={disabled}>{t("tools.hostsModeText")}</TabsTrigger>
            </TabsList>
          </Tabs>
        </div>
        {(hosts.error || siteQuery.error) && <div role="alert" className="text-xs text-error [overflow-wrap:anywhere]">
          {t("tools.hostsReadFailed")} {normalizeError(hosts.error ?? siteQuery.error).message}
        </div>}
        {!ready && !hosts.error && !siteQuery.error && <p role="status" className="text-xs text-muted">{t("common.loading")}</p>}
        {!deleteTarget && !clearOpen && errorBox}
        {mode === "list" ? <>
          <div className="max-h-64 overflow-y-auto rounded-lg bg-fill/50" aria-busy={hosts.isFetching}>
            {ready && entries.length === 0 && <p className="px-3 py-5 text-center text-xs text-faint">{t("tools.hostsEmpty")}</p>}
            {entries.map((entry, index) => <div key={`${entry.ip}|${entry.domain}|${entry.managed}|${index}`}
              className="mx-3 flex flex-wrap items-center gap-x-2 gap-y-1 border-b border-dashed border-separator py-3 last:border-0">
              <div className="min-w-0 flex-1 basis-36 space-y-1">
                <p className="break-all font-mono text-xs">{entry.domain}</p>
                <p className="break-all font-mono text-[11px] text-muted">{entry.ip}</p>
              </div>
              <Badge variant={editable(entry) ? "default" : "muted"}>{siteDomains.has(entry.domain) && entry.managed ? t("tools.hostsSite") : entry.managed ? t("tools.hostsManual") : t("tools.hostsSystem")}</Badge>
              {editable(entry) && <div className="flex shrink-0 gap-1">
                <Button size="icon-sm" variant="ghost" disabled={disabled} aria-label={`${t("tools.hostsEdit")} ${entry.domain}`}
                  onClick={() => { setEditing(entry); setNewDomain(entry.domain); setNewIp(entry.ip); setError(null); formRef.current?.querySelector("input")?.focus(); }}>
                  <FilePenLine className="h-3.5 w-3.5" />
                </Button>
                <Button size="icon-sm" variant="ghost" disabled={disabled} aria-label={`${t("common.delete")} ${entry.domain}`} className="text-faint hover:text-error"
                  onClick={() => { setError(null); setDeleteTarget(entry); }}><Trash2 className="h-3.5 w-3.5" /></Button>
              </div>}
            </div>)}
          </div>
          <p className="text-[11px] leading-relaxed text-muted">{t("tools.hostsSiteHint")}</p>
          <div ref={formRef} className="grid min-w-0 gap-2 sm:grid-cols-2">
            <div className="min-w-0 space-y-1.5"><Label htmlFor="hosts-ip">{t("tools.hostsIp")}</Label>
              <Input id="hosts-ip" value={newIp} disabled={disabled} onChange={(e) => setNewIp(e.target.value)} className="min-w-0 font-mono" placeholder="127.0.0.1 / ::1" /></div>
            <div className="min-w-0 space-y-1.5"><Label htmlFor="hosts-domain">{t("tools.hostsDomain")}</Label>
              <Input id="hosts-domain" value={newDomain} disabled={disabled} onChange={(e) => setNewDomain(e.target.value)} className="min-w-0 font-mono" placeholder="newsite.test"
                onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void saveEntry(); } }} /></div>
            <div className="flex flex-wrap gap-2 sm:col-span-2">
              <Button variant="secondary" disabled={disabled || !newDomain.trim() || !newIp.trim()} onClick={saveEntry}>{editing ? t("common.save") : t("tools.add")}</Button>
              {editing && <Button variant="ghost" disabled={busy} onClick={resetForm}>{t("common.cancel")}</Button>}
            </div>
          </div>
        </> : <>
          <Label htmlFor="hosts-text">{t("tools.hostsModeText")}</Label>
          <p className="text-[11px] leading-relaxed text-muted">{t("tools.hostsTextHint")}</p>
          <textarea id="hosts-text" value={textDraft} disabled={busy} onChange={(e) => setTextDraft(e.target.value)}
            rows={8} spellCheck={false} style={paletteVars}
            className="nsb-code w-full min-w-0 rounded-lg border border-border p-3 font-mono text-[11.5px] leading-relaxed outline-none focus:border-border-strong"
            placeholder={"127.0.0.1 newsite.test alias.test\n::1 ipv6.test"} />
          <div className="flex flex-wrap gap-2">
            <Button variant="secondary" size="sm" disabled={disabled} onClick={() => applyText()}>{t("tools.hostsApplyText")}</Button>
            <Button variant="ghost" size="sm" disabled={busy} onClick={() => { setMode("list"); setError(null); }}>{t("common.cancel")}</Button>
          </div>
        </>}
      </div>
      <ConfirmDialog open={!!deleteTarget} onOpenChange={(open) => { if (!open && !action.current) setDeleteTarget(null); }}
        title={`${t("common.delete")} ${deleteTarget?.domain ?? ""}`} description={t("tools.hostsDeleteHint")}
        confirmText={t("common.delete")} danger loading={busy} confirmDisabled={!ready} onConfirm={remove}>{errorBox}</ConfirmDialog>
      <ConfirmDialog open={clearOpen} onOpenChange={(open) => { if (!action.current) setClearOpen(open); }}
        title={t("tools.hostsClearTitle")} description={t("tools.hostsClearHint")} confirmText={t("common.delete")}
        danger loading={busy} confirmDisabled={!ready} onConfirm={() => applyText(true)}>{errorBox}</ConfirmDialog>
    </ToolCard>
  );
}


/* ============ 端口查询 / 结束进程 ============ */
function PortLookupTool() {
  const t = useT();
  const [port, setPort] = React.useState("");
  const [range, setRange] = React.useState({ from: "", to: "" });
  const [busy, setBusy] = React.useState(false);
  const [rows, setRows] = React.useState<ListenerInfo[]>([]);
  const [scanned, setScanned] = React.useState<string>("");
  const [confirmTarget, setConfirmTarget] = React.useState<ListenerInfo | null>(null);
  const invalidate = useInvalidate();
  const { data: settings } = useSettings();
  const confirmKill = settings?.confirmKill ?? true;

  const scan = React.useCallback(
    async (from: number, to: number) => {
      setBusy(true);
      try {
        const r = await api.scanPortRange(from, to);
        setRows(r.listeners);
        setScanned(from === to ? `:${from}` : `${from}–${to}`);
      } catch (e) {
        toastError(e);
      } finally {
        setBusy(false);
      }
    },
    []
  );

  /** 结束占用者：本应用服务会被优雅停止，外部进程直接被结束 */
  const closePort = async (row: ListenerInfo) => {
    setBusy(true);
    try {
      const outcome = await api.closePort(row.port);
      if (outcome.graceful) {
        toast.success(`${t("tools.portFreedService")} ${outcome.serviceId ?? row.serviceId ?? ""}`.trim());
      } else {
        toast.success(
          `${t("tools.killedP1")} ${row.processName ?? "PID " + row.pid} ${t("tools.killedP2")}`,
          { description: `:${row.port} ${t("tools.portFreed")}` }
        );
      }
      invalidate("services");
      setConfirmTarget(null);
      // 重新扫一遍确认端口真的空出来了
      const r = await api.scanPortRange(row.port, row.port);
      setRows((prev) => [...prev.filter((p) => p.port !== row.port), ...r.listeners]);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const doKill = (row: ListenerInfo) => {
    if (!confirmKill) {
      void closePort(row);
      return;
    }
    setConfirmTarget(row);
  };

  return (
    <>
      <ToolCard icon={Power} title={t("tools.portLookup")} hint={t("tools.portLookupHint")}>
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-end gap-2">
            <div className="flex flex-1 flex-col gap-1">
              <label className="text-[10.5px] text-faint">{t("tools.portSingle")}</label>
              <Input
                value={port}
                onChange={(e) => setPort(e.target.value.replace(/[^\d]/g, ""))}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && port) scan(Number(port), Number(port));
                }}
                placeholder="3000"
                className="h-8 w-28 font-mono text-[12px]"
              />
            </div>
            <div className="flex flex-1 flex-col gap-1">
              <label className="text-[10.5px] text-faint">{t("tools.portRange")}</label>
              <div className="flex items-center gap-1">
                <Input
                  value={range.from}
                  onChange={(e) => setRange({ ...range, from: e.target.value.replace(/[^\d]/g, "") })}
                  placeholder="8000"
                  className="h-8 w-20 font-mono text-[12px]"
                />
                <span className="text-faint">–</span>
                <Input
                  value={range.to}
                  onChange={(e) => setRange({ ...range, to: e.target.value.replace(/[^\d]/g, "") })}
                  placeholder="8100"
                  className="h-8 w-20 font-mono text-[12px]"
                />
              </div>
            </div>
            <Button
              variant="secondary"
              size="sm"
              className="h-8"
              disabled={busy}
              onClick={() => {
                if (port) {
                  scan(Number(port), Number(port));
                  return;
                }
                const from = Number(range.from);
                const to = Number(range.to) || from;
                if (!from) {
                  toast.error(t("tools.portInputHint"));
                  return;
                }
                scan(Math.min(from, to), Math.max(from, to));
              }}
            >
              {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Search className="h-3.5 w-3.5" />}
              {t("tools.scan")}
            </Button>
          </div>

          {/* 常用端口快捷入口 */}
          <div className="flex flex-wrap gap-1">
            {COMMON_PORTS.map((p) => (
              <button
                key={p}
                type="button"
                onClick={() => {
                  setPort(String(p));
                  scan(p, p);
                }}
                className="rounded-md bg-fill px-1.5 py-0.5 font-mono text-[10.5px] text-faint transition-colors hover:border-border-strong hover:text-secondary"
              >
                {p}
              </button>
            ))}
          </div>

          <div className="max-h-64 overflow-y-auto rounded-md bg-fill">
            {rows.length === 0 ? (
              <p className="px-3 py-5 text-center text-[11px] text-faint">
                {busy ? t("tools.scanning") : scanned ? t("tools.portLookupEmpty") : t("tools.portLookupIdle")}
              </p>
            ) : (
              rows.map((r) => (
                <div
                  key={`${r.port}-${r.pid}`}
                  className="flex items-center gap-2 border-b border-border/60 px-3 py-2 last:border-0"
                >
                  <span className={cn("h-2 w-2 shrink-0 rounded-full", r.ownedBySelf ? "bg-success" : "bg-error")} />
                  <code className="w-16 shrink-0 font-mono text-[11.5px]">:{r.port}</code>
                  <span className="min-w-0 flex-1 truncate text-[11.5px]">
                    <b className={r.ownedBySelf ? "text-secondary" : "text-error"}>
                      {r.processName ?? t("tools.unknown")}
                    </b>
                    <span className="text-faint"> (PID {r.pid})</span>
                    {r.ownedBySelf && r.serviceId && (
                      <span className="text-success"> · {t("tools.portSelf")} {r.serviceId}</span>
                    )}
                    {r.cmdline && <span className="mt-0.5 block truncate text-[10.5px] text-faint">{r.cmdline}</span>}
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="shrink-0 text-error hover:text-error"
                    disabled={busy}
                    onClick={() => doKill(r)}
                  >
                    {r.ownedBySelf ? t("tools.stopService") : t("tools.endProcess")}
                  </Button>
                </div>
              ))
            )}
          </div>
          {scanned && rows.length > 0 && (
            <p className="text-[10.5px] text-faint">
              {scanned} · {rows.length} {t("tools.listeners")}
            </p>
          )}
        </div>
      </ToolCard>

      <ConfirmDialog
        open={confirmTarget !== null}
        onOpenChange={(open) => !open && setConfirmTarget(null)}
        title={
          confirmTarget?.ownedBySelf
            ? `${t("tools.stopService")} · ${confirmTarget.serviceId ?? ""}`
            : `${t("tools.endProcess")} · ${confirmTarget?.processName ?? ""}`
        }
        description={
          confirmTarget?.ownedBySelf
            ? t("tools.stopServiceConfirm")
            : t("tools.killConfirm")
        }
        danger={!confirmTarget?.ownedBySelf}
        loading={busy}
        confirmText={confirmTarget?.ownedBySelf ? t("common.stop") : t("tools.endProcess")}
        onConfirm={() => confirmTarget && closePort(confirmTarget)}
      />
    </>
  );
}

/* ============ 端口体检 ============ */
function PortTool() {
  const t = useT();
  const [custom, setCustom] = React.useState("");
  const [scanning, setScanning] = React.useState(false);
  const [appRows, setAppRows] = React.useState<PortScanEntry[]>([]);
  const [results, setResults] = React.useState<PortDiagnosis[]>([]);
  const [killTarget, setKillTarget] = React.useState<{ pid: number; name?: string } | null>(null);
  const [killing, setKilling] = React.useState(false);
  const killAfter = React.useRef<(() => void) | null>(null);
  const invalidate = useInvalidate();
  const { data: settings } = useSettings();
  const confirmKill = settings?.confirmKill ?? true;

  /** 体检本应用需要的全部端口（一次后端调用，含占用者与结论） */
  const scanApp = React.useCallback(async () => {
    setScanning(true);
    try {
      setAppRows(await api.scanPorts());
    } catch (e) {
      toastError(e);
    } finally {
      setScanning(false);
    }
  }, []);

  /** 额外扫用户指定/常见端口 */
  const scanCustom = async () => {
    setScanning(true);
    const ports = [
      ...COMMON_PORTS,
      ...custom.split(/[,，\s]+/).map(Number).filter((n) => n > 0 && n < 65536),
    ];
    const unique = [...new Set(ports)];
    const found: PortDiagnosis[] = [];
    for (const p of unique) {
      try {
        const d = await api.diagnosePort(p);
        if (d.inUse) found.push(d);
      } catch {
        /* ignore */
      }
    }
    setResults(found);
    setScanning(false);
  };

  React.useEffect(() => {
    scanApp();
  }, [scanApp]);

  /** 确认框点「结束进程」后执行 */
  const execKill = async () => {
    if (!killTarget) return;
    setKilling(true);
    try {
      await api.killPid(killTarget.pid);
      toast.success(`${t("tools.killedP1")} ${killTarget.name ?? killTarget.pid} ${t("tools.killedP2")}`);
      invalidate("services");
      killAfter.current?.();
    } catch (e) {
      toastError(e);
    } finally {
      setKilling(false);
      setKillTarget(null);
    }
  };

  /** 结束占用进程：开了「结束进程前二次确认」就弹确认框，关了直接执行 */
  const doKill = (pid: number, name?: string, after?: () => void) => {
    if (!confirmKill) {
      setKilling(true);
      api
        .killPid(pid)
        .then(() => {
          toast.success(`${t("tools.killedP1")} ${name ?? pid} ${t("tools.killedP2")}`);
          invalidate("services");
          after?.();
        })
        .catch(toastError)
        .finally(() => setKilling(false));
      return;
    }
    killAfter.current = after ?? null;
    setKillTarget({ pid, name });
  };

  const conflicts = appRows.filter((r) => r.verdict === "conflict");

  return (
    <ToolCard icon={Radar} title={t("tools.ports")} hint={t("tools.portsTitle")}>
      <div className="flex flex-col gap-3">
        {/* 一键体检：本应用端口 vs 占用者 */}
        <div className="flex items-center justify-between gap-2">
          <span className="text-[11.5px] text-muted">{t("tools.portCheckApp")}</span>
          <Button variant="secondary" size="sm" onClick={scanApp} disabled={scanning}>
            {scanning ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
            {t("tools.portCheckRun")}
          </Button>
        </div>
        {conflicts.length > 0 && (
          <p className="rounded-md border border-error/30 bg-error/10 px-2.5 py-1.5 text-[11px] text-error">
            {conflicts.length} {t("tools.portConflictsP2")}
          </p>
        )}
        <div className="max-h-56 overflow-y-auto rounded-md bg-fill">
          {appRows.length === 0 ? (
            <p className="px-3 py-4 text-center text-[11px] text-faint">
              {scanning ? t("tools.scanning") : t("tools.portsEmpty")}
            </p>
          ) : (
            appRows.map((r) => (
              <div
                key={`${r.serviceId}-${r.port}-${r.label}`}
                className="flex items-center gap-2 border-b border-border/60 px-3 py-2 last:border-0"
              >
                {r.verdict === "conflict" ? (
                  <XCircle className="h-3.5 w-3.5 shrink-0 text-error" />
                ) : (
                  <span
                    className={cn(
                      "h-2 w-2 shrink-0 rounded-full",
                      r.verdict === "self" ? "bg-success" : "bg-faint/40"
                    )}
                  />
                )}
                <code className="w-14 shrink-0 font-mono text-[11.5px]">:{r.port}</code>
                <span className="flex-1 truncate text-[11.5px]">
                  <span className="text-faint">{r.label}</span>
                  {r.verdict === "conflict" && (
                    <>
                      {" — "}
                      {t("tools.who")} <b className="text-error">{r.processName ?? t("tools.unknown")}</b>
                      {r.pid ? <span className="text-faint"> (PID {r.pid})</span> : null}
                    </>
                  )}
                  {r.verdict === "self" && <span className="text-success"> · {t("tools.portSelf")}</span>}
                  {r.verdict === "free" && <span className="text-faint"> · {t("tools.portFree")}</span>}
                </span>
                {r.verdict === "conflict" && r.pid && (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-error hover:text-error"
                    onClick={() => doKill(r.pid!, r.processName, scanApp)}
                  >
                    {t("tools.kill")}
                  </Button>
                )}
              </div>
            ))
          )}
        </div>

        {/* 任意端口自查 */}
        <div className="flex gap-2 border-t border-border/60 pt-3">
          <Input
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
            placeholder={t("tools.portsPlaceholder")}
            className="font-mono"
          />
          <Button variant="secondary" onClick={scanCustom} disabled={scanning}>
            {t("tools.scan")}
          </Button>
        </div>
        {results.length > 0 && (
          <div className="max-h-40 overflow-y-auto rounded-md bg-fill">
            {results.map((r) => (
              <div key={r.port} className="flex items-center gap-2 border-b border-border/60 px-3 py-2 last:border-0">
                <XCircle className="h-3.5 w-3.5 text-error" />
                <code className="w-14 shrink-0 font-mono text-[11.5px]">:{r.port}</code>
                <span className="flex-1 truncate text-[11.5px]">
                  {t("tools.who")} <b className="text-error">{r.processName ?? t("tools.unknown")}</b>
                  {r.pid ? <span className="text-faint"> (PID {r.pid})</span> : null}
                </span>
                {r.pid && (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-error hover:text-error"
                    onClick={() => doKill(r.pid!, r.processName, scanCustom)}
                  >
                    {t("tools.kill")}
                  </Button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* 结束进程二次确认（可在设置里关掉） */}
      <ConfirmDialog
        open={killTarget !== null}
        onOpenChange={(o) => !o && setKillTarget(null)}
        title={t("confirm.killProcess")}
        description={t("confirm.killProcessDesc")
          .replace("{name}", killTarget?.name ?? t("tools.unknown"))
          .replace("{pid}", String(killTarget?.pid ?? ""))}
        confirmText={t("tools.endProcess")}
        danger
        loading={killing}
        onConfirm={execKill}
      />
    </ToolCard>
  );
}

/* ============ 终端注入 ============ */
function TerminalInjectTool() {
  const t = useT();
  const [script, setScript] = React.useState("");
  React.useEffect(() => {
    // 路径与版本都从后端取；shell 语法按平台切换（PowerShell / zsh·bash）
    Promise.all([
      api.getDataDir().catch(() => ""),
      api.listPackages().catch(() => []),
    ])
      .then(([dataDir, packages]) => {
        const installed = packages.filter((p) => p.install);
        const php = installed.find((p) => p.id === "php");
        const mysql = installed.find((p) => p.id === "mysql");
        if (!dataDir) return;
        const isWin = /Win/i.test(navigator.userAgent);
        const rt = `${dataDir}/runtimes`;
        const sep = isWin ? "\\" : "/";
        const join = (...parts: string[]) => parts.join(sep);
        const lines = installed.map((p) => {
          const bin = join(rt, p.id, p.version);
          const withBin =
            p.id === "mysql" || p.id === "postgresql"
              ? join(bin, p.id === "mysql" ? "bin" : "bin")
              : bin;
          return isWin
            ? `$env:PATH = "${withBin};$env:PATH"`
            : `export PATH="${withBin}:$PATH"`;
        });
        if (lines.length === 0) {
          setScript(`# ${t("tools.termInjectNone")}`);
          return;
        }
        const checks = [php ? "php -v" : "", mysql ? "mysql --version" : ""].filter(Boolean).join("; ");
        const header = isWin
          ? `# ${t("tools.termInjectPsHeader")}`
          : `# ${t("tools.termInjectShHeader")}`;
        setScript([header, ...lines, checks].filter(Boolean).join("\n"));
      })
      .catch(() => undefined);
  }, []);
  return (
    <ToolCard icon={SquareTerminal} title={t("tools.termInject")} hint={t("tools.termInjectHint")}>
      <div className="flex flex-col gap-2">
        <CodeBlock code={script} lang="shell" maxHeight={176} compact />
        <div className="flex gap-2">
          <Button
            variant="secondary"
            size="sm"
            onClick={() => api.openTerminal(".").catch(toastError)}
          >
            {t("dashboard.openTerminal")}
          </Button>
        </div>
      </div>
    </ToolCard>
  );
}

/* ============ 伪静态模板 ============ */
const REWRITE_SNIPPETS: Record<string, string> = {
  Laravel: `location / {
    try_files $uri $uri/ /index.php?$query_string;
}`,
  Symfony: `location / {
    try_files $uri $uri/ /index.php$is_args$args;
}`,
  ThinkPHP: `location / {
    if (!-e $request_filename) {
        rewrite ^(.*)$ /index.php?s=$1 last;
    }
}`,
  WordPress: `location / {
    try_files $uri $uri/ /index.php?$args;
}
rewrite /wp-admin$ $scheme://$host$uri/ permanent;`,
  Yii2: `location / {
    try_files $uri $uri/ /index.php?$args;
}`,
  "CodeIgniter 4": `location / {
    try_files $uri $uri/ /index.php$is_args$args;
}
location ~* ^/(app|system|writable)/ {
    deny all;
}`,
  CakePHP: `location / {
    try_files $uri $uri/ /index.php?url=$uri&$args;
}`,
  Drupal: `location / {
    try_files $uri $uri/ /index.php?$query_string;
}
location ~* \.(engine|inc|info|install|module|profile|po|sh|.*sql|theme|tpl(\.php)?|xtmpl)$ {
    deny all;
}`,
  Joomla: `location / {
    try_files $uri $uri/ /index.php?$args;
}`,
  "SPA fallback": `location / {
    try_files $uri $uri/ /index.html;
}`,
  "Next.js export": `location / {
    try_files $uri $uri/ /index.html;
}`,
};

function RewriteTemplates() {
  const t = useT();
  return (
    <ToolCard icon={FileCode2} title={t("tools.rewriteTemplates")} hint={t("tools.rewriteTemplatesHint")}>
      <div className="flex flex-col gap-2">
        {Object.entries(REWRITE_SNIPPETS).map(([name, code]) => (
          <CodeBlock key={name} title={name} code={code} lang="nginx" compact showLineNumbers={false} />
        ))}
      </div>
    </ToolCard>
  );
}

/* ============ 备份 ============ */
function BackupTool() {
  const t = useT();
  const backups = useQuery({ queryKey: ["backups"], queryFn: api.listBackups });
  const [search, setSearch] = React.useState("");
  const [restoreTarget, setRestoreTarget] = React.useState<string | null>(null);
  const filtered = (backups.data ?? []).filter((backup) =>
    `${backup.targetPath ?? ""} ${backup.name}`.toLowerCase().includes(search.trim().toLowerCase())
  );

  return (
    <ToolCard icon={DatabaseBackup} title={t("tools.backup")} hint={t("tools.backupHint")}>
      <div className="flex flex-col gap-3">
        <Input aria-label={t("tools.backupSearch")} placeholder={t("tools.backupSearch")} value={search} onChange={(e) => setSearch(e.target.value)} />
        {backups.error && <p role="alert" className="break-words text-xs text-error">{normalizeError(backups.error).message}</p>}
        <div className="max-h-72 overflow-y-auto rounded-md bg-fill" aria-busy={backups.isFetching}>
          {filtered.length === 0 ? backups.error ? null : (
            <p className="px-3 py-4 text-center text-[11px] text-faint">
              {backups.isPending ? t("common.loading") : search.trim() ? t("tools.noBackupMatches") : t("tools.noBackups")}
            </p>
          ) : (
            filtered.map((b) => (
              <div key={b.name} className="flex items-start gap-2 border-b border-border/60 px-3 py-3 last:border-0">
                <div className="min-w-0 flex-1 space-y-1">
                  <p className="break-all font-mono text-[11.5px]">{b.targetPath ?? b.name}</p>
                  <p className="text-[10.5px] text-faint">{new Date(b.modifiedAt).toLocaleString()} · {formatBytes(b.sizeBytes)}</p>
                  {b.targetPath && <p className="break-all text-[10px] text-faint">{b.name}</p>}
                  {b.reason && <p className="break-words text-[11px] text-muted">{b.reason}</p>}
                </div>
                <Button size="sm" variant="ghost" className="min-h-9 shrink-0" disabled={!b.restorable || !!backups.error} onClick={() => setRestoreTarget(b.name)}>{t("tools.restore")}</Button>
              </div>
            ))
          )}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" size="sm" onClick={() => void backups.refetch()} disabled={backups.isFetching}>
            {backups.error ? t("install.retry") : t("tools.refresh")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              api.getDataDir().then((dir) => api.openInFolder(`${dir}/backup`))
                .then(() => toast.success(t("tools.backupOpened")))
                .catch(toastError);
            }}
          >
            {t("tools.openBackupDirBtn")}
          </Button>
        </div>
      </div>

      {restoreTarget && <BackupRestoreDialog name={restoreTarget} onClose={() => setRestoreTarget(null)} />}
    </ToolCard>
  );
}

function BackupRestoreDialog({ name, onClose }: { name: string; onClose: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const [opener] = React.useState(() => document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const preview = useQuery({ queryKey: ["backup-preview", name], queryFn: () => api.previewBackup(name), retry: false, staleTime: 0, gcTime: 0, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const action = React.useRef(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const restore = async () => {
    if (action.current || !preview.data || preview.isFetching || preview.error || error) return;
    action.current = true;
    setBusy(true);
    try {
      await api.restoreBackup(name, preview.data.revision);
      invalidate("backups", "config-files");
      toast.success(t("tools.restored"), { description: t("tools.backupRestoreHint") });
      onClose();
    } catch (e) {
      setError(normalizeError(e).message);
    } finally {
      action.current = false;
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !action.current) onClose(); }}>
      <DialogContent hideClose={busy} onCloseAutoFocus={(e) => { e.preventDefault(); requestAnimationFrame(() => opener?.focus()); }} className="flex max-h-[calc(100dvh-1.5rem)] flex-col overflow-hidden p-4 sm:p-6">
        <DialogHeader className="shrink-0 pr-7">
          <DialogTitle>{t("confirm.restoreBackup")}</DialogTitle>
          <DialogDescription>{t("tools.backupRestoreHint")}</DialogDescription>
        </DialogHeader>
        <div className="min-h-0 space-y-3 overflow-y-auto text-xs">
          <p className="break-all font-mono text-faint">{name}</p>
          {preview.isFetching ? <p role="status">{t("common.loading")}</p> : preview.data && (
            <div className="space-y-1 rounded-lg bg-fill p-3"><p className="text-muted">{t("tools.backupTarget")}</p><p className="break-all font-mono">{preview.data.targetPath}</p></div>
          )}
          {(error || preview.error) && (
            <div className="space-y-2"><p role="alert" className="break-words text-error">{error ?? normalizeError(preview.error).message}</p>
              <Button size="sm" variant="secondary" disabled={busy || preview.isFetching} onClick={async () => { const result = await preview.refetch(); if (!result.error) setError(null); }}>{t("tools.retryPreview")}</Button>
            </div>
          )}
        </div>
        <DialogFooter className="shrink-0">
          <Button variant="secondary" disabled={busy} onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="destructive" disabled={busy || preview.isFetching || !preview.data || !!preview.error || !!error} onClick={restore}>
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}{t("tools.restore")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function formatBytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/* ============ 修复向导 ============ */
function RepairTool() {
  const t = useT();
  const invalidate = useInvalidate();
  const [busy, setBusy] = React.useState<string | null>(null);
  const action = React.useRef(false);
  const [resetOpen, setResetOpen] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const items = [
    {
      id: "configs",
      label: t("tools.rebuildConf"),
      desc: t("tools.rebuildConfHint"),
      action: async () => {
        setResetOpen(true);
      },
    },
    {
      id: "validate",
      label: t("tools.validateConf"),
      desc: t("tools.validateConfHint"),
      action: async () => {
        const checks = await api.validateConfigs();
        const fails = checks.filter((c) => !c.ok);
        if (fails.length > 0) {
          throw new Error(fails.map((c) => `${c.name}: ${c.detail}`).join("；"));
        }
        toast.success(
          `${t("tools.validateOkP1")} ${checks.filter((c) => c.status === "ok").length} ${t("tools.validateOkP2")}`
        );
      },
    },
    {
      id: "hosts",
      label: t("tools.rebuildHosts"),
      desc: t("tools.rebuildHostsHint"),
      action: async () => {
        // 传 null 语义：让后端按「站点域名 + 用户自定义条目」重建，
        // 而不是用空数组把用户手动条目清掉
        await api.rebuildHosts();
      },
    },
    {
      id: "certs",
      label: t("tools.rebuildCerts"),
      desc: t("tools.rebuildCertsHint"),
      action: async () => {
        // 逐个站点补签缺失/过期证书；不再签发一张名字叫 rebuild-all 的假证书
        await api.reissueSiteCerts();
      },
    },
  ];
  return (
    <ToolCard icon={Wrench} title={t("tools.fixWizard")} hint={t("tools.wizardHint")}>
      <div className="flex flex-col gap-2">
        {error && <p role="alert" className="break-words text-xs text-error">{error}</p>}
        {items.map((item) => (
          <div key={item.id} className="flex items-center justify-between gap-3 rounded-md bg-fill p-3">
            <div className="min-w-0">
              <p className="text-[12px] font-medium">{item.label}</p>
              <p className="text-[10.5px] text-faint">{item.desc}</p>
            </div>
            <Button
              size="sm"
              variant="secondary"
              className="min-h-9 shrink-0"
              disabled={busy !== null || resetOpen}
              onClick={async () => {
                if (action.current) return;
                action.current = true;
                setBusy(item.id);
                setError(null);
                try {
                  await item.action();
                  if (item.id !== "configs") {
                    if (item.id !== "validate") toast.success(`${item.label} ${t("tools.wizardDoneP2")}`);
                    invalidate("services", "hosts", "certs");
                  }
                } catch (e) {
                  setError(normalizeError(e).message);
                } finally {
                  action.current = false;
                  setBusy(null);
                }
              }}
            >
              {busy === item.id ? <Loader2 className="h-3 w-3 animate-spin" /> : null}
              {t("tools.run")}
            </Button>
          </div>
        ))}
      </div>
      {resetOpen && <ResetConfigDialog onClose={() => setResetOpen(false)} />}
    </ToolCard>
  );
}

function ResetConfigDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const [opener] = React.useState(() => document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const files = useQuery({ queryKey: ["config-files"], queryFn: api.configList });
  const targets = (files.data ?? []).filter((file) => file.resettable);
  const [selected, setSelected] = React.useState("");
  const kind = targets.some((file) => file.kind === selected) ? selected : targets[0]?.kind ?? "";
  const preview = useQuery({ queryKey: ["config-reset-preview", kind], queryFn: () => api.configResetPreview(kind), enabled: !!kind, retry: false, staleTime: 0, gcTime: 0, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const action = React.useRef(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const reset = async () => {
    if (action.current || !preview.data?.changed || preview.isFetching || preview.error || error || files.error) return;
    action.current = true;
    setBusy(true);
    try {
      const result = await api.configReset(kind, preview.data.revision);
      invalidate("backups", "config-files");
      toast.success(t("tools.resetDone"), { description: result.usedByService ? t("cfgeditor.restartHint").replace("{s}", result.usedByService) : undefined });
      onClose();
    } catch (e) {
      setError(normalizeError(e).message);
    } finally {
      action.current = false;
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !action.current) onClose(); }}>
      <DialogContent hideClose={busy} onCloseAutoFocus={(e) => { e.preventDefault(); requestAnimationFrame(() => opener?.focus()); }} className="flex max-h-[calc(100dvh-1.5rem)] max-w-2xl flex-col overflow-hidden p-4 sm:p-6">
        <DialogHeader className="shrink-0 pr-7">
          <DialogTitle>{t("tools.rebuildConf")}</DialogTitle>
          <DialogDescription>{t("tools.configResetDesc")}</DialogDescription>
        </DialogHeader>
        <div className="min-h-0 space-y-4 overflow-y-auto text-xs">
          {files.isPending ? <p role="status">{t("common.loading")}</p> : files.error ? (
            <div className="space-y-2"><p role="alert" className="break-words text-error">{normalizeError(files.error).message}</p><Button variant="secondary" size="sm" onClick={() => void files.refetch()} disabled={files.isFetching}>{t("install.retry")}</Button></div>
          ) : targets.length === 0 ? <p className="text-muted">{t("tools.noResetTargets")}</p> : (
            <>
              <div className="space-y-2">
                <label htmlFor="reset-config-target" className="font-medium">{t("tools.configTarget")}</label>
                <Select value={kind} onValueChange={(value) => { setSelected(value); setError(null); }} disabled={busy}>
                  <SelectTrigger id="reset-config-target" className="w-full"><SelectValue /></SelectTrigger>
                  <SelectContent>{targets.map((file) => <SelectItem key={file.kind} value={file.kind}>{file.label}</SelectItem>)}</SelectContent>
                </Select>
              </div>
              {preview.isFetching ? <p role="status">{t("common.loading")}</p> : preview.data && (
                <>
                  <p className="break-all rounded-lg bg-fill p-3 font-mono text-muted">{preview.data.path}</p>
                  {!preview.data.changed && <p role="status" className="text-success">{t("tools.configAlreadyDefault")}</p>}
                  <details className="min-w-0 rounded-lg border border-border p-3">
                    <summary className="cursor-pointer rounded-sm font-medium focus-visible:outline focus-visible:outline-2">{t("tools.defaultPreview")}</summary>
                    <div className="mt-3 min-w-0 overflow-hidden">
                      <CodeBlock code={preview.data.content} lang={preview.data.language === "ini" ? "ini" : preview.data.language === "nginx" ? "nginx" : "plain"} compact maxHeight={240} />
                    </div>
                  </details>
                </>
              )}
              {(error || preview.error) && <div className="space-y-2">
                <p role="alert" className="break-words text-error">{error ?? normalizeError(preview.error).message}</p>
                <Button variant="secondary" size="sm" disabled={busy || preview.isFetching} onClick={async () => { const result = await preview.refetch(); if (!result.error) setError(null); }}>{t("tools.retryPreview")}</Button>
              </div>}
            </>
          )}
        </div>
        <DialogFooter className="shrink-0">
          <Button variant="secondary" disabled={busy} onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="destructive" disabled={busy || !kind || !preview.data?.changed || preview.isFetching || !!preview.error || !!files.error || !!error} onClick={reset}>
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}{t("tools.rebuildConf")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}


/* ============ 本地域名解析（CoreDNS） ============ */
function DnsTool() {
  const t = useT();
  const services = useServices(3000);
  const settings = useSettings();
  const dns = services.data.find((service) => service.id === "coredns");
  const running = dns?.state === "running";
  const port = dns?.port;
  const tld = settings.data?.defaultTld || "test";
  const [activeIf, setActiveIf] = React.useState("");
  const interfaces = useQuery({ queryKey: ["dns-interfaces"], queryFn: api.dnsInterfaces, retry: false });
  const selected = activeIf && interfaces.data?.includes(activeIf) ? activeIf : interfaces.data?.[0] ?? "";
  const status = useQuery({ queryKey: ["dns-interface", selected], queryFn: () => api.dnsStatusOf(selected), enabled: !!selected, retry: false });
  const [busy, setBusy] = React.useState(false);
  const action = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [confirm, setConfirm] = React.useState<"takeover" | "restore" | null>(null);
  const serviceReady = services.dataUpdatedAt > 0 && !services.error;
  const interfaceReady = !!selected && status.isSuccess && !status.error && !interfaces.error;
  const changing = dns?.state === "starting" || dns?.state === "stopping";
  const canTakeover = serviceReady && running && port === 53 && interfaceReady && !settings.error
    && !status.data?.local && !status.data?.backup;
  const canRestore = interfaceReady && !!(status.data?.backup || status.data?.local);
  const formatConfig = (config: api.DnsConfiguration) =>
    `${config.automatic ? t("dns.automatic") : t("dns.static")} ${config.servers.join(", ")}`.trim();
  const refresh = async () => {
    await interfaces.refetch();
    if (selected) await status.refetch();
  };
  const run = async (operation: () => Promise<void>) => {
    if (action.current) return;
    action.current = true;
    setBusy(true);
    setError(null);
    try { await operation(); }
    catch (e) { setError(normalizeError(e)); }
    finally {
      await Promise.all([services.refetch(), selected ? status.refetch() : Promise.resolve()]);
      action.current = false;
      setBusy(false);
    }
  };
  const toggle = () => run(async () => {
    if (!dns || !serviceReady) return;
    if (running) await api.stopService(dns.id);
    else await api.startService(dns.id);
  });
  const changeInterface = () => run(async () => {
    if (!interfaceReady || !confirm) return;
    if (confirm === "takeover") {
      if (!canTakeover) return;
      await api.dnsTakeover(selected);
      toast.success(`${t("dns.takeoverDoneP1")} ${selected} ${t("dns.takeoverDoneP2")}`);
    } else {
      await api.dnsRestore(selected, !status.data?.backup);
      toast.success(t("dns.restoreDone"));
    }
    setConfirm(null);
  });
  const errorBox = error && <div role="alert" className="space-y-1 rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">
    <p>{error.message}</p>{error.hint && <p>{error.hint}</p>}
  </div>;
  const stateLabel = !serviceReady ? t("dns.stateUnknown") : !dns ? t("dns.notInstalled")
    : running ? t("dns.running") : dns.state === "error" ? t("state.error")
    : changing ? t(dns.state === "starting" ? "state.starting" : "state.stopping") : t("dns.stopped");

  return (
    <ToolCard icon={Radar} title={t("dns.title")} hint={t("dns.hint").replace("{tld}", tld)}>
      <div className="flex min-w-0 flex-col gap-3">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 flex-1 basis-44">
            <p role="status" className="text-[12.5px] font-medium">{stateLabel}
              {port != null && <span className="ml-2 font-mono text-[11px] text-faint">:{port}</span>}
            </p>
            <p className="mt-1 text-[11px] leading-relaxed text-muted">{t("dns.stateHint")}</p>
          </div>
          {dns ? <Button variant="secondary" size="sm" disabled={busy || changing || !serviceReady} onClick={toggle}>
            {busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{running ? t("dns.stop") : t("dns.start")}
          </Button> : serviceReady && <a href="/packages" className="rounded-md px-2 py-1.5 text-xs text-primary hover:underline">{t("dns.install")}</a>}
          {services.error && <Button size="sm" variant="ghost" onClick={() => void services.refetch()} disabled={services.isFetching}>{t("bulk.retry")}</Button>}
        </div>
        {(services.error || settings.error) && <p role="alert" className="break-words text-xs text-error">{normalizeError(services.error ?? settings.error).message}</p>}
        {dns?.lastError && <p role="alert" className="break-words text-xs text-error">{dns.lastError.message}</p>}
        {!confirm && errorBox}
        <div className="space-y-3 border-y border-dashed border-separator py-3">
          <div className="space-y-1">
            <p className="text-[12px] font-medium">{t("dns.takeoverTitle")}</p>
            <p className="text-[11px] leading-relaxed text-muted">{t("dns.takeoverHint")}</p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <div className="min-w-0 flex-1 basis-40">
              <Label htmlFor="dns-interface">{t("dns.interface")}</Label>
              <Select value={selected} onValueChange={(name) => { setActiveIf(name); setError(null); }} disabled={busy || !!confirm || interfaces.isFetching}>
                <SelectTrigger id="dns-interface" className="mt-1 w-full min-w-0"><SelectValue placeholder={t("dns.interface")} /></SelectTrigger>
                <SelectContent>{interfaces.data?.map((name) => <SelectItem key={name} value={name}>{name}</SelectItem>)}</SelectContent>
              </Select>
            </div>
            <Button size="sm" variant="ghost" disabled={busy || !!confirm || interfaces.isFetching || status.isFetching} onClick={() => void refresh()}>{t("dns.refresh")}</Button>
          </div>
          {interfaces.error ? <p role="alert" className="break-words text-xs text-error">{t("dns.readFailed")}: {normalizeError(interfaces.error).message}</p>
            : interfaces.isPending ? <p role="status" className="text-xs text-muted">{t("dns.statusLoading")}</p>
            : !interfaces.data?.length ? <p className="text-xs text-muted">{t("dns.noInterfaces")}</p> : null}
          {selected && <div className="space-y-1.5 rounded-lg bg-fill p-3 text-xs">
            <p className="text-muted">{t("dns.interfaceStatus")}</p>
            {status.error ? <p role="alert" className="break-words text-error">{normalizeError(status.error).message}</p>
              : status.isPending ? <p role="status">{t("dns.statusLoading")}</p>
              : status.data && <>
                <p className="break-all font-mono">{formatConfig(status.data.current)}</p>
                {status.data.backup && <><p className="pt-1 text-muted">{t("dns.original")}</p><p className="break-all font-mono">{formatConfig(status.data.backup)}</p></>}
                {status.data.local && !status.data.backup && <p className="text-muted">{t("dns.legacy")}</p>}
              </>}
          </div>}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="secondary" disabled={busy || !canTakeover || !!confirm} onClick={() => { setError(null); setConfirm("takeover"); }}>{t("dns.takeover")}</Button>
            <Button size="sm" variant="ghost" disabled={busy || !canRestore || !!confirm} onClick={() => { setError(null); setConfirm("restore"); }}>
              {status.data?.backup ? t("dns.restore") : t("dns.restoreAutomatic")}
            </Button>
          </div>
          {!running || port !== 53 ? <p className="text-[11px] leading-relaxed text-muted">{t("dns.requireReady")}</p> : null}
        </div>
        <div className="space-y-1.5 text-[11px] leading-relaxed text-muted">
          <p className="font-medium text-secondary">{t("dns.setupStep2")}</p>
          <code className="block break-all rounded-lg bg-fill p-2 font-mono">nslookup -port={port ?? 53} niceenv-check.{tld} 127.0.0.1</code>
          <p>{t("dns.stopHint")}</p>
        </div>
      </div>
      <ConfirmDialog open={!!confirm} onOpenChange={(open) => { if (!open && !action.current) setConfirm(null); }}
        title={`${confirm === "takeover" ? t("dns.takeover") : t("dns.restore")} · ${selected}`}
        description={confirm === "takeover" ? t("dns.confirmTakeover") : status.data?.backup ? t("dns.confirmRestore") : t("dns.confirmAutomatic")}
        confirmText={confirm === "takeover" ? t("dns.takeover") : t("dns.restore")} loading={busy}
        confirmDisabled={confirm === "takeover" ? !canTakeover : !canRestore} onConfirm={changeInterface}>
        {status.data && <div className="rounded-lg bg-fill p-3 text-xs [overflow-wrap:anywhere]">
          {confirm === "takeover" ? "127.0.0.1" : status.data.backup ? formatConfig(status.data.backup) : t("dns.automatic")}
        </div>}
        {errorBox}
      </ConfirmDialog>
    </ToolCard>
  );
}
