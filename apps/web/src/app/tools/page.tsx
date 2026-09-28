"use client";

import * as React from "react";
import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
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
import type { ListenerInfo, PortRangeScan, ClosePortOutcome, HostsEntry, ConfigFileInfo } from "@nsb/schema";
import { HostsEntry as HostsEntrySchema } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useHosts, useInvalidate, toastError, useSettings, useSites, useServices, copyText } from "@/lib/hooks";
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
import { ConfigEditor, ConfigEditDialog } from "@/components/shared/config-editor";
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
  const [portRequest, setPortRequest] = React.useState<{ port: number; at: number } | null>(null);
  const inspectPort = (port: number) => { setPortRequest({ port, at: Date.now() }); document.getElementById("nsb-tool-ports")?.scrollIntoView({ block: "start", behavior: "smooth" }); };
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
        <div id="nsb-tool-ports" className="min-w-0 scroll-mt-6"><PortLookupTool request={portRequest} /></div>
        <DnsTool />
        <HostsTool />
        <PortTool onInspect={inspectPort} />
        <div id="nsb-tool-pathenv" className="min-w-0 scroll-mt-6"><PathEnvCard /></div>
        <TerminalInjectTool />
        <RewriteTemplates />
        <div id="nsb-tool-config">
          <ConfigEditor />
        </div>
        <div id="nsb-tool-diagnostics" className="min-w-0">
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
    <Card className="min-w-0">
      <CardHeader className="flex-row flex-wrap items-start gap-3 p-3 sm:p-5">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill">
          <Icon className="h-4 w-4 shrink-0 text-primary" strokeWidth={1.8} />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="text-[13px]">{title}</CardTitle>
          <CardDescription className="mt-0.5 line-clamp-2 text-[11px] leading-relaxed">{hint}</CardDescription>
        </div>
        {action && <div className="flex min-w-0 flex-wrap gap-2">{action}</div>}
      </CardHeader>
      <CardContent className="min-w-0 p-3 pt-0 sm:p-5 sm:pt-0">{children}</CardContent>
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
function PortLookupTool({ request }: { request: { port: number; at: number } | null }) {
  const t = useT();
  const qc = useQueryClient();
  const [mode, setMode] = React.useState<"single" | "range">("single");
  const [port, setPort] = React.useState("");
  const [range, setRange] = React.useState({ from: "", to: "" });
  const [report, setReport] = React.useState<PortRangeScan | null>(null);
  const [inputError, setInputError] = React.useState("");
  const [outcome, setOutcome] = React.useState<ClosePortOutcome | null>(null);
  const [confirmTarget, setConfirmTarget] = React.useState<ListenerInfo | null>(null);
  const action = React.useRef(false);
  const inputRef = React.useRef<HTMLInputElement>(null);
  const closeTrigger = React.useRef<HTMLButtonElement | null>(null);
  const scanButtonRef = React.useRef<HTMLButtonElement>(null);
  const { data: settings } = useSettings();
  const busy = useIsMutating({ mutationKey: ["ports"] }) > 0;
  const scan = useMutation({
    mutationKey: ["ports", "lookup"], networkMode: "always",
    mutationFn: ({ from, to }: { from: number; to: number }) => api.scanPortRange(from, to),
    onSuccess: (data) => { setReport(data); setOutcome(null); close.reset(); },
    onSettled: () => { action.current = false; },
  });
  const close = useMutation({
    mutationKey: ["ports", "close"], networkMode: "always",
    mutationFn: (row: ListenerInfo) => api.closePort(row.port, [row]),
    onSuccess: (data) => {
      setOutcome(data); setConfirmTarget(null);
      setReport((previous) => previous ? { ...previous, listeners: [...previous.listeners.filter((row) => row.port !== data.port), ...data.remaining].sort((a, b) => a.port - b.port || a.pid - b.pid) } : previous);
    },
    onSettled: () => { action.current = false; void qc.invalidateQueries({ queryKey: ["services"] }); void qc.invalidateQueries({ queryKey: ["ports", "app"] }); },
  });
  const runScan = (fromText: string, toText = fromText) => {
    if (action.current || busy) return;
    const from = Number(fromText); const to = Number(toText);
    if (![fromText, toText].every((text) => /^\d+$/.test(text)) || ![from, to].every((n) => Number.isInteger(n) && n >= 1 && n <= 65535)) {
      setInputError(t("tools.portValid")); return;
    }
    setInputError(""); action.current = true;
    scan.mutate({ from: Math.min(from, to), to: Math.max(from, to) });
  };
  const requested = React.useRef(request);
  React.useEffect(() => {
    if (!request || request === requested.current || busy) return;
    requested.current = request;
    setMode("single"); setPort(String(request.port)); setInputError("");
    action.current = true; scan.mutate({ from: request.port, to: request.port });
    inputRef.current?.focus({ preventScroll: true });
  }, [request, busy, scan.mutate]);
  const doClose = (row: ListenerInfo) => {
    if (busy || action.current || !row.canClose) return;
    action.current = true; close.mutate(row);
  };
  const error = scan.error || close.error;
  const details = error ? normalizeError(error) : null;
  const hasFailure = !!details;
  const submit = () => mode === "single" ? runScan(port) : runScan(range.from, range.to || range.from);
  return <>
    <ToolCard icon={Power} title={t("tools.portLookup")} hint={t("tools.portLookupHint")}>
      <div className="min-w-0 space-y-3" aria-busy={busy}>
        <p className="text-[11px] leading-relaxed text-muted">{t("tools.portTcpScope")}</p>
        {!isTauri && <p className="rounded-md bg-warn/10 p-2 text-[11px] text-warn">{t("tools.portDemo")}</p>}
        <Tabs value={mode} onValueChange={(value) => setMode(value as "single" | "range")}>
          <TabsList><TabsTrigger value="single">{t("tools.portSingle")}</TabsTrigger><TabsTrigger value="range">{t("tools.portRange")}</TabsTrigger></TabsList>
        </Tabs>
        <form className="flex min-w-0 flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); submit(); }}>
          {mode === "single" ? <div className="min-w-0 flex-1 space-y-1">
            <Label htmlFor="port-lookup-single">{t("tools.portSingle")}</Label>
            <Input id="port-lookup-single" ref={inputRef} inputMode="numeric" value={port} onChange={(e) => setPort(e.target.value)} placeholder="3000" className="font-mono" aria-invalid={!!inputError} aria-describedby={inputError ? "port-input-error" : undefined} />
          </div> : <div className="flex min-w-0 flex-1 gap-2">
            <div className="min-w-0 flex-1 space-y-1"><Label htmlFor="port-range-from">{t("tools.portFrom")}</Label><Input id="port-range-from" inputMode="numeric" value={range.from} onChange={(e) => setRange({ ...range, from: e.target.value })} placeholder="8000" className="font-mono" /></div>
            <div className="min-w-0 flex-1 space-y-1"><Label htmlFor="port-range-to">{t("tools.portTo")}</Label><Input id="port-range-to" inputMode="numeric" value={range.to} onChange={(e) => setRange({ ...range, to: e.target.value })} placeholder="8100" className="font-mono" /></div>
          </div>}
          <Button ref={scanButtonRef} type="submit" variant="secondary" disabled={busy}>{scan.isPending ? <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" /> : <Search className="h-3.5 w-3.5" />}{t("tools.scan")}</Button>
        </form>
        {inputError && <p id="port-input-error" role="alert" className="text-xs text-error">{inputError}</p>}
        <div className="flex flex-wrap gap-1">{COMMON_PORTS.map((value) => <button key={value} type="button" disabled={busy} onClick={() => { setMode("single"); setPort(String(value)); runScan(String(value)); }} className="min-h-8 rounded-md bg-fill px-2 font-mono text-[11px] text-muted hover:text-foreground disabled:opacity-50">{value}</button>)}</div>
        {details && <div role="alert" className="space-y-1 rounded-lg border border-error/30 p-3 text-xs text-error [overflow-wrap:anywhere]"><p>{details.message}</p>{details.hint && <p>{details.hint}</p>}{report && <p>{t("tools.portPrevious")}</p>}<Button variant="ghost" size="sm" disabled={busy} onClick={() => report ? runScan(String(report.from), String(report.to)) : submit()}>{t("tools.refresh")}</Button></div>}
        {outcome && <div role="status" className={cn("rounded-lg border p-3 text-xs [overflow-wrap:anywhere]", outcome.portFree && !outcome.errors.length ? "border-success/30 text-success" : "border-warn/30 text-warn")}>
          <p>{outcome.portFree ? t("tools.portFreed") : t("tools.portStillBusy")} · :{outcome.port}</p>
          <p className="mt-1">{t("tools.portProcessed").replace("{count}", String(outcome.killedPids.length)).replace("{remaining}", String(outcome.remaining.length))}</p>
          <p className="mt-1">{t("tools.portOutcomeScope")}</p>
          {outcome.errors.map((message, index) => <p className="mt-1" key={index}>{message}</p>)}
        </div>}
        {scan.isPending && <p role="status" className="text-xs text-muted">{t("tools.scanning")}{report ? ` · ${t("tools.portPrevious")}` : ""}</p>}
        <div className="max-h-80 overflow-y-auto rounded-lg bg-fill px-3">
          {!report ? <p className="py-5 text-center text-xs text-muted">{t(scan.isPending ? "tools.scanning" : "tools.portLookupIdle")}</p> : report.listeners.length === 0 ? <p className="py-5 text-center text-xs text-muted">{t("tools.portLookupEmpty")}</p> : report.listeners.map((row) => <div key={`${row.port}-${row.pid}`} className="space-y-2 border-b border-dashed border-border py-3 last:border-0">
            <div className="flex min-w-0 flex-col items-start gap-2 sm:flex-row sm:justify-between">
              <div className="min-w-0 flex-1 [overflow-wrap:anywhere]"><p className="text-xs font-medium"><code>:{row.port}</code> · {row.processName || t("tools.unknown")}</p><p className="mt-1 text-[11px] text-muted">PID {row.pid} · {t(`tools.portOwner.${row.ownership}`)}{row.serviceId ? ` · ${row.serviceId}` : ""}</p></div>
              <Button size="sm" variant="ghost" className="h-auto min-h-8 whitespace-normal text-error" disabled={busy || hasFailure || !row.canClose} onClick={(event) => { closeTrigger.current = event.currentTarget; close.reset(); if (settings?.confirmKill === false) doClose(row); else setConfirmTarget(row); }}>{row.ownedBySelf ? t("tools.stopService") : t("tools.endProcess")}</Button>
            </div>
            {row.cmdline && <details className="text-[11px] text-muted"><summary className="cursor-pointer">{t("tools.portCommand")}</summary><pre className="mt-1 whitespace-pre-wrap font-mono [overflow-wrap:anywhere]">{row.cmdline}</pre></details>}
            {row.closeReason && <p className="text-[11px] text-warn [overflow-wrap:anywhere]">{row.closeReason}</p>}
          </div>)}
        </div>
        {report && <p className="text-[10.5px] leading-relaxed text-muted">:{report.from}{report.to !== report.from ? `–${report.to}` : ""} · {report.listeners.length} {t("tools.listeners")} · <time dateTime={new Date(report.scannedAt).toISOString()}>{new Date(report.scannedAt).toLocaleString()}</time></p>}
      </div>
    </ToolCard>
    <ConfirmDialog open={!!confirmTarget} onCloseAutoFocus={(event) => { event.preventDefault(); (closeTrigger.current?.isConnected ? closeTrigger.current : scanButtonRef.current)?.focus(); }} onOpenChange={(open) => { if (!open && !close.isPending) setConfirmTarget(null); }} title={confirmTarget?.ownedBySelf ? t("tools.stopService") : t("tools.endProcess")} description={confirmTarget?.ownedBySelf ? t("tools.stopServiceConfirm") : t("tools.killConfirm")} danger={!confirmTarget?.ownedBySelf} loading={close.isPending} confirmDisabled={!!close.error || (busy && !close.isPending)} confirmText={confirmTarget?.ownedBySelf ? t("common.stop") : t("tools.endProcess")} onConfirm={() => { if (confirmTarget) doClose(confirmTarget); }}>
      {confirmTarget && <p className="text-xs [overflow-wrap:anywhere]">:{confirmTarget.port} · {confirmTarget.processName || t("tools.unknown")} · PID {confirmTarget.pid}{confirmTarget.serviceId ? ` · ${confirmTarget.serviceId}` : ""}</p>}
      {close.error && <p role="alert" className="mt-2 text-xs text-error [overflow-wrap:anywhere]">{normalizeError(close.error).message}<span className="mt-1 block">{t("tools.portRescanRequired")}</span></p>}
    </ConfirmDialog>
  </>;
}

/* ============ 端口体检 ============ */
function PortTool({ onInspect }: { onInspect: (port: number) => void }) {
  const t = useT();
  const [custom, setCustom] = React.useState("");
  const [inputError, setInputError] = React.useState("");
  const [customReport, setCustomReport] = React.useState<{ ports: number[]; result: PortRangeScan } | null>(null);
  const action = React.useRef(false);
  const busy = useIsMutating({ mutationKey: ["ports"] }) > 0;
  const app = useQuery({ queryKey: ["ports", "app"], queryFn: api.scanPorts, retry: false, networkMode: "always", refetchOnWindowFocus: false });
  const scan = useMutation({ mutationKey: ["ports", "custom"], networkMode: "always", mutationFn: async (ports: number[]) => {
    const result = await api.scanPortRange(Math.min(...ports), Math.max(...ports));
    return { ports, result: { ...result, listeners: result.listeners.filter((row) => ports.includes(row.port)) } };
  }, onSuccess: setCustomReport, onSettled: () => { action.current = false; } });
  const scanCustom = () => {
    if (action.current || busy) return;
    const values = custom.trim() ? custom.trim().split(/[,，\s]+/) : [];
    if (values.some((value) => !/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 65535) || values.length > 32) { setInputError(t("tools.portCustomValid")); return; }
    setInputError(""); action.current = true;
    scan.mutate([...new Set([...COMMON_PORTS, ...values.map(Number)])]);
  };
  return <ToolCard icon={Radar} title={t("tools.ports")} hint={t("tools.portsTitle")}>
    <div className="min-w-0 space-y-3">
      <p className="text-[11px] leading-relaxed text-muted">{t("tools.portAppScope")}</p>
      {!isTauri && <p className="text-[11px] text-warn">{t("tools.portDemo")}</p>}
      <div className="flex flex-wrap items-center justify-between gap-2"><span className="text-xs text-muted">{t("tools.portCheckApp")}</span><Button variant="secondary" size="sm" disabled={busy || app.isFetching} onClick={() => void app.refetch({ cancelRefetch: false })}>{app.isFetching && <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />}{t("tools.portCheckRun")}</Button></div>
      {app.error && <div role="alert" className="rounded-lg border border-error/30 p-3 text-xs text-error [overflow-wrap:anywhere]">{normalizeError(app.error).message}{app.data && <p className="mt-1">{t("tools.portPrevious")}</p>}</div>}
      {app.isPending && <p role="status" className="text-xs text-muted">{t("tools.scanning")}</p>}
      {app.data && <div className="max-h-72 overflow-y-auto rounded-lg bg-fill px-3">{app.data.length === 0 ? <p className="py-4 text-xs text-muted">{t("tools.portNoServices")}</p> : app.data.map((row) => <div key={`${row.serviceId}-${row.port}-${row.label}`} className="space-y-1 border-b border-dashed border-border py-3 last:border-0">
        <div className="flex min-w-0 flex-col items-start gap-2 sm:flex-row sm:justify-between"><div className="min-w-0 flex-1 text-xs [overflow-wrap:anywhere]"><p><code>:{row.port}</code> · {row.label}</p><p className={cn("mt-1", row.verdict === "self" ? "text-success" : ["conflict", "missing", "unknown"].includes(row.verdict) ? "text-warn" : "text-muted")}>{t(`tools.portVerdict.${row.verdict}`)}{row.listenerCount ? ` · ${row.listenerCount} ${t("tools.listeners")}` : ""}</p></div><Button size="sm" variant="ghost" disabled={busy || app.isFetching} className="h-auto min-h-8 whitespace-normal" onClick={() => onInspect(row.port)}>{t("tools.portInspect")}</Button></div>
        <p className="text-[10.5px] leading-relaxed text-muted [overflow-wrap:anywhere]">{row.detail}</p>
      </div>)}</div>}
      {app.dataUpdatedAt > 0 && <p className="text-[10.5px] text-muted">{t("svc.diag.checkedAt")} {new Date(app.dataUpdatedAt).toLocaleString()}</p>}
      <div className="mx-1 border-t border-dashed border-border" />
      <form className="space-y-2" onSubmit={(event) => { event.preventDefault(); scanCustom(); }}><Label htmlFor="ports-custom">{t("tools.portCustom")}</Label><div className="flex min-w-0 flex-wrap gap-2"><Input id="ports-custom" className="min-w-0 flex-1 basis-36 font-mono" value={custom} onChange={(event) => setCustom(event.target.value)} placeholder="3000, 5173" /><Button variant="secondary" disabled={busy} type="submit">{t("tools.scan")}</Button></div></form>
      {inputError && <p role="alert" className="text-xs text-error">{inputError}</p>}
      {scan.error && <div role="alert" className="text-xs text-error [overflow-wrap:anywhere]">{normalizeError(scan.error).message}{customReport && <p>{t("tools.portPrevious")}</p>}</div>}
      {scan.isPending && <p role="status" className="text-xs text-muted">{t("tools.scanning")}</p>}
      {customReport && <div className="rounded-lg bg-fill px-3 text-xs"><p className="py-3 text-[11px] text-muted [overflow-wrap:anywhere]">{t("tools.portScanned")} {customReport.ports.join(", ")} · {new Date(customReport.result.scannedAt).toLocaleTimeString()}</p>{customReport.result.listeners.length === 0 ? <p className="pb-3 text-muted">{t("tools.portLookupEmpty")}</p> : customReport.result.listeners.map((row) => <div key={`${row.port}-${row.pid}`} className="flex min-w-0 flex-wrap items-center justify-between gap-2 border-t border-dashed border-border py-2"><span className="min-w-0 flex-1 [overflow-wrap:anywhere]">:{row.port} · {row.processName || t("tools.unknown")} · PID {row.pid}</span><Button variant="ghost" size="sm" disabled={busy} onClick={() => onInspect(row.port)}>{t("tools.portInspect")}</Button></div>)}</div>}
    </div>
  </ToolCard>;
}

/* ============ 终端注入 ============ */
function TerminalInjectTool() {
  const t = useT();
  const environment = useQuery({
    queryKey: ["pathenv", "terminal"],
    queryFn: () => api.terminalEnvironment(),
    staleTime: 5_000,
    refetchInterval: 15_000,
    retry: false,
  });
  const [opening, setOpening] = React.useState(false);
  const openingRef = React.useRef(false);
  const data = environment.data;
  const error = environment.error ? normalizeError(environment.error) : null;
  const open = async () => {
    if (!data || openingRef.current) return;
    openingRef.current = true;
    setOpening(true);
    try {
      await api.openTerminal(data.revision);
    } catch (error) {
      toastError(error);
    } finally {
      openingRef.current = false;
      setOpening(false);
    }
  };
  return (
    <div id="nsb-tool-terminal" className="min-w-0">
      <ToolCard icon={SquareTerminal} title={t("tools.termInject")} hint={t("tools.termInjectHint")}>
        <div className="flex min-w-0 flex-col gap-3" aria-busy={environment.isFetching}>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="secondary" size="sm" disabled={environment.isFetching} onClick={() => void environment.refetch()}>
              <RefreshCw className={cn("h-3.5 w-3.5", environment.isFetching && "animate-spin")} />
              {t("tools.refresh")}
            </Button>
            <Button variant="ghost" size="sm" className="h-auto whitespace-normal text-left" onClick={() => {
              document.getElementById("nsb-tool-pathenv")?.scrollIntoView({ behavior: "smooth", block: "center" });
            }}>{t("tools.termInjectVersions")}</Button>
          </div>
          {environment.isPending && <p role="status" className="text-[12px] text-muted">{t("common.loading")}</p>}
          {error && <div role="alert" className="break-words rounded-lg border border-error/30 bg-error/5 p-3 text-[12px] text-error">
            <p>{error.message}</p>
            {error.hint && <p className="mt-1">{error.hint}</p>}
            <Button variant="outline" size="sm" className="mt-2" disabled={environment.isFetching} onClick={() => void environment.refetch()}>{t("sites.retry")}</Button>
          </div>}
          {data && !error && <>
            <p className="text-[11px] leading-relaxed text-muted">{t("tools.termInjectScope")}</p>
            {!isTauri && <p className="rounded-lg bg-warn/10 p-2 text-[11px] leading-relaxed text-warn">{t("tools.termInjectDemo")}</p>}
            {data.warnings.length > 0 && <div role="status" className="rounded-lg border border-warn/30 p-2.5 text-[11px] text-warn">
              <p className="font-medium">{t("tools.termInjectSkipped")}</p>
              <ul className="mt-1 list-disc space-y-1 pl-4 [overflow-wrap:anywhere]">{data.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul>
            </div>}
            {data.entries.length > 0 ? <>
              <div className="max-h-40 space-y-2 overflow-auto rounded-lg border border-border p-2.5">
                {data.entries.map((entry) => <div key={entry.id} className="min-w-0">
                  <p className="flex flex-wrap items-center gap-x-2 text-[12px]"><span className="font-medium">{entry.label}</span><span className="font-mono text-[11px] text-muted">{entry.version}</span></p>
                  <p className="mt-0.5 break-all font-mono text-[10.5px] text-faint">{entry.binDir}</p>
                </div>)}
              </div>
              <CodeBlock code={data.script} lang="shell" title={data.shell === "powershell" ? "PowerShell" : "Bash / Zsh"} maxHeight={240} compact />
            </> : <p className="rounded-lg bg-fill p-3 text-[12px] leading-relaxed text-muted">{t("tools.termInjectNone")}</p>}
            <div className="flex flex-wrap gap-2">
              <Button variant="secondary" size="sm" className="h-auto whitespace-normal py-2 text-left" disabled={opening || environment.isFetching || !isTauri} onClick={() => void open()}>
                {opening && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                {data.shell === "powershell" ? t("tools.termInjectOpenPs") : t("dashboard.openTerminal")}
              </Button>
            </div>
          </>}
        </div>
      </ToolCard>
    </div>
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
  const queryClient = useQueryClient();
  const checks = useQuery({ queryKey: ["config-checks"], queryFn: () => api.validateConfigs(), enabled: false, retry: false });
  const files = useQuery({ queryKey: ["config-files"], queryFn: api.configList });
  const checking = useIsMutating({ mutationKey: ["config-checks"] }) > 0;
  const validation = useMutation({
    mutationKey: ["config-checks"],
    mutationFn: (only: string[] | undefined) => api.validateConfigs(only),
    onSuccess: (result, only) => {
      queryClient.setQueryData<api.ConfigCheck[]>(["config-checks"], (previous) => {
        if (!only || !previous) return result;
        const byKind = new Map(result.map((row) => [row.kind, row]));
        return previous.map((row) => byKind.get(row.kind) ?? row);
      });
      void queryClient.invalidateQueries({ queryKey: ["config-files"] });
    },
  });
  const [busy, setBusy] = React.useState<string | null>(null);
  const action = React.useRef(false);
  const [resetOpen, setResetOpen] = React.useState(false);
  const [resetKind, setResetKind] = React.useState<string | undefined>();
  const [editing, setEditing] = React.useState<ConfigFileInfo | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [issuesOnly, setIssuesOnly] = React.useState(false);
  const reportRef = React.useRef<HTMLDivElement>(null);
  const errorRef = React.useRef<HTMLParagraphElement>(null);
  const locked = busy !== null || checking || resetOpen || !!editing;
  const rows = checks.data ?? [];
  const failures = rows.filter((row) => row.status === "fail");
  const issues = rows.filter((row) => row.status === "fail" || row.status === "warning");
  const visibleRows = issuesOnly ? issues : rows;
  const statusLabel = (row: api.ConfigCheck) => t(row.status === "skipped" ? "tools.checkSkipped" : row.status === "fail" ? "tools.checkFailed" : row.status === "warning" ? "tools.checkWarning" : row.method === "readability" ? "tools.checkReadable" : "tools.checkPassed");
  const reportText = rows.map((row) => `${row.name} · ${statusLabel(row)} · ${new Date(row.checkedAt).toLocaleString()}\n${row.path ?? ""}\n${row.detail}`).join("\n\n");
  const run = async (id: string, fn: () => Promise<unknown>) => {
    if (action.current || queryClient.isMutating({ mutationKey: ["config-checks"] }) || resetOpen || editing) return;
    action.current = true;
    setBusy(id);
    setError(null);
    try {
      await fn();
      if (id === "hosts" || id === "certs") invalidate("services", "hosts", "certs");
      if (id === "validate") requestAnimationFrame(() => reportRef.current?.focus());
    } catch (error) {
      setError(normalizeError(error).message);
      requestAnimationFrame(() => errorRef.current?.focus());
    } finally {
      action.current = false;
      setBusy(null);
    }
  };
  const items = [
    { id: "configs", label: t("tools.rebuildConf"), desc: t("tools.rebuildConfHint"), action: async () => { setResetKind(undefined); setResetOpen(true); } },
    { id: "validate", label: t("tools.validateConf"), desc: t("tools.validateConfHint"), action: () => validation.mutateAsync(undefined) },
    { id: "hosts", label: t("tools.rebuildHosts"), desc: t("tools.rebuildHostsHint"), action: async () => {
      // 后端保留用户手动条目，只同步站点域名。
      await api.rebuildHosts();
      toast.success(`${t("tools.rebuildHosts")} ${t("tools.wizardDoneP2")}`);
    } },
    { id: "certs", label: t("tools.rebuildCerts"), desc: t("tools.rebuildCertsHint"), action: async () => {
      const issued = await api.reissueSiteCerts();
      toast.success(issued.length ? t("tools.checkCertsRenewed").replace("{n}", String(issued.length)) : t("tools.checkCertsUnchanged"));
    } },
  ];
  return (
    <div id="nsb-tool-repair" className="min-w-0">
      <ToolCard icon={Wrench} title={t("tools.fixWizard")} hint={t("tools.wizardHint")}>
        <div className="flex min-w-0 flex-col gap-3">
          {error && <p ref={errorRef} tabIndex={-1} role="alert" className="break-words rounded-lg border border-error/30 p-3 text-xs text-error">{error}</p>}
          {items.map((item) => (
            <div key={item.id} className="flex flex-wrap items-center justify-between gap-2 rounded-md bg-fill p-3">
              <div className="min-w-0 flex-1 basis-40">
                <p className="text-[12px] font-medium">{item.label}</p>
                <p className="mt-0.5 text-[10.5px] leading-relaxed text-muted">{item.desc}</p>
              </div>
              <Button size="sm" variant="secondary" className="min-h-9 shrink-0" aria-label={`${item.label} · ${t("tools.run")}`} disabled={locked} onClick={() => void run(item.id, item.action)}>
                {(busy === item.id || (item.id === "validate" && checking)) && <Loader2 className="h-3 w-3 animate-spin" />}
                {t("tools.run")}
              </Button>
            </div>
          ))}
          {(checking || rows.length > 0) && <div ref={reportRef} tabIndex={-1} className="min-w-0 space-y-3 rounded-lg border border-border p-3 outline-none focus-visible:ring-2 focus-visible:ring-primary" aria-label={t("tools.checkReport")}>
            <p className="text-[12px] font-medium">{t("tools.checkReport")}</p>
            <p className="text-[11px] leading-relaxed text-muted">{t("tools.checkScope")}</p>
            {!isTauri && <p className="text-[11px] text-warn">{t("tools.checkDemo")}</p>}
            {checking && <p role="status" className="flex items-start gap-2 text-[11px] text-muted"><Loader2 className="mt-0.5 h-3.5 w-3.5 shrink-0 animate-spin" />{t("tools.checkRunning")}</p>}
            {rows.length > 0 && <>
              <div role="status" className="flex flex-wrap gap-x-3 gap-y-1 text-[11px]">
                <span>{t("tools.checkPassed")} {rows.filter((row) => row.status === "ok" && row.method === "native").length}</span>
                <span>{t("tools.checkReadable")} {rows.filter((row) => row.status === "ok" && row.method === "readability").length}</span>
                <span className="text-warn">{t("tools.checkWarning")} {rows.filter((row) => row.status === "warning").length}</span>
                <span className="text-error">{t("tools.checkFailed")} {failures.length}</span>
                <span className="text-muted">{t("tools.checkSkipped")} {rows.filter((row) => row.status === "skipped").length}</span>
              </div>
              <p className="text-[10.5px] leading-relaxed text-muted">{t("tools.checkSnapshot")}</p>
              <div className="flex flex-wrap items-center gap-2">
                <Button size="sm" variant={issuesOnly ? "secondary" : "ghost"} aria-pressed={issuesOnly} onClick={() => setIssuesOnly((value) => !value)}>{t("tools.checkIssuesOnly")}</Button>
                <Button size="sm" variant="secondary" className="h-auto whitespace-normal py-2 text-left" disabled={locked || failures.length === 0} onClick={() => void run("validate", () => validation.mutateAsync(failures.map((row) => row.kind)))}>{t("tools.checkRetryFailed")}</Button>
                <Button size="sm" variant="ghost" onClick={() => void copyText(reportText)}>{t("tools.checkCopy")}</Button>
              </div>
              <div className="max-h-[28rem] space-y-3 overflow-auto">
                {visibleRows.length === 0 && <p className="py-2 text-[11px] text-muted">{t("tools.checkNoIssues")}</p>}
                {visibleRows.map((row) => {
                  const file = files.data?.find((file) => file.kind === row.kind);
                  return <div key={row.kind} className="min-w-0 space-y-2 border-t border-dashed border-border pt-3">
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <p className="break-words text-[12px] font-medium">{row.name}</p>
                      <Badge variant={row.status === "fail" ? "error" : row.status === "warning" ? "warn" : row.status === "ok" && row.method === "native" ? "running" : "muted"}>{statusLabel(row)}</Badge>
                    </div>
                    {row.path && <p className="break-all font-mono text-[10px] text-muted">{row.path}</p>}
                    <p className="text-[10px] text-faint">{new Date(row.checkedAt).toLocaleString()}</p>
                    <details open={row.status === "fail" || row.status === "warning"}>
                      <summary className="cursor-pointer text-[11px] text-muted">{t("tools.checkDetails")}</summary>
                      <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-all text-[10.5px] leading-relaxed">{row.detail}</pre>
                    </details>
                    {row.method !== "none" && <div className="flex flex-wrap gap-2">
                      <Button size="sm" variant="ghost" disabled={locked} onClick={() => void run("validate", () => validation.mutateAsync([row.kind]))}>{t("tools.checkAgain")}</Button>
                      {file?.exists && <Button size="sm" variant="ghost" disabled={locked || !!files.error} onClick={() => setEditing(file)}>{t("tools.checkEdit")}</Button>}
                      {file && !file.exists && file.resettable && <Button size="sm" variant="ghost" className="h-auto whitespace-normal py-2 text-left" disabled={locked || !!files.error} onClick={() => { setResetKind(file.kind); setResetOpen(true); }}>{t("tools.checkGenerate")}</Button>}
                    </div>}
                  </div>;
                })}
              </div>
            </>}
          </div>}
        </div>
        {resetOpen && <ResetConfigDialog initialKind={resetKind} onClose={() => setResetOpen(false)} />}
        {editing && <ConfigEditDialog info={editing} onClose={() => setEditing(null)} onSaved={() => { void files.refetch(); }} />}
      </ToolCard>
    </div>
  );
}

function ResetConfigDialog({ onClose, initialKind }: { onClose: () => void; initialKind?: string }) {
  const t = useT();
  const invalidate = useInvalidate();
  const [opener] = React.useState(() => document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const files = useQuery({ queryKey: ["config-files"], queryFn: api.configList });
  const targets = (files.data ?? []).filter((file) => file.resettable);
  const [selected, setSelected] = React.useState(initialKind ?? "");
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
