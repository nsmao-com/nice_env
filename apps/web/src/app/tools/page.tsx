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
} from "lucide-react";
import type { ListenerInfo, PortDiagnosis, PortScanEntry , HostsEntry } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useHosts, useInvalidate, toastError, useSettings, useSites, useServices } from "@/lib/hooks";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
  const { data: entries } = useHosts();
  const { data: sites } = useSites();
  const invalidate = useInvalidate();
  const [newDomain, setNewDomain] = React.useState("");
  const [newIp, setNewIp] = React.useState("127.0.0.1");
  const [busy, setBusy] = React.useState(false);
  /** list = 逐条管理；text = 整段文本编辑（从 hosts 文件粘贴也行） */
  const [mode, setMode] = React.useState<"list" | "text">("list");
  const [textDraft, setTextDraft] = React.useState("");

  /** 站点域名由站点配置自动重建，删不掉；其余托管条目（用户手动加的）可以删 */
  const siteDomains = React.useMemo(
    () => new Set(sites.flatMap((s) => s.domains)),
    [sites]
  );
  const removable = (e: { domain: string; managed: boolean }) => e.managed && !siteDomains.has(e.domain);

  const apply = async (next: HostsEntry[]) => {
    setBusy(true);
    try {
      await api.applyHosts(next);
      invalidate("hosts");
    } catch (e) {
      toastError(e, t("tools.hostsWriteFailed"));
      throw e;
    } finally {
      setBusy(false);
    }
  };

  const remove = async (target: { ip: string; domain: string }) => {
    try {
      await apply(entries.filter((e) => !(e.domain === target.domain && e.ip === target.ip)));
      toast.success(t("tools.hostsDeleted"));
    } catch {
      /* toastError 已弹 */
    }
  };

  const add = async () => {
    const domain = newDomain.trim();
    if (!domain) return;
    try {
      await apply([...entries, { ip: newIp.trim() || "127.0.0.1", domain, managed: true }]);
      toast.success(`${t("tools.hostsUpdatedP1")}${domain}`);
      setNewDomain("");
    } catch {
      /* 已弹 */
    }
  };

  /* ---- 文本模式 ---- */
  const openText = () => {
    // 只编辑托管段；系统自有条目不属于本工具的管理范围
    const managed = entries
      .filter((e) => e.managed)
      .map((e) => `${e.ip} ${e.domain}`)
      .join("\n");
    setTextDraft(managed);
    setMode("text");
  };

  const applyText = async () => {
    const parsed: HostsEntry[] = [];
    for (const raw of textDraft.split("\n")) {
      const line = raw.trim();
      if (!line || line.startsWith("#")) continue;
      const parts = line.split(/\s+/);
      if (parts.length < 2) continue;
      const ip = parts[0];
      const domain = parts[1];
      if (!/^[\d.:a-fA-F]+$/.test(ip)) continue; // IPv4/IPv6 字面量
      parsed.push({ ip, domain, managed: true });
    }
    // 非托管的系统条目原样保留
    const system = entries.filter((e) => !e.managed);
    try {
      await apply([...system, ...parsed]);
      toast.success(`${t("tools.hostsUpdatedP1")}${parsed.length}`);
      setMode("list");
    } catch {
      /* 已弹 */
    }
  };

  /* ---- 文件导入 ---- */
  const importFile = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({
        multiple: false,
        filters: [{ name: "hosts", extensions: ["hosts", "txt", "conf"] }],
      });
      if (!path || typeof path !== "string") return;
      const raw = await api.readTextFile(path);
      const parsed: HostsEntry[] = [];
      for (const rawLine of raw.split(/\r?\n/)) {
        const line = rawLine.trim();
        if (!line || line.startsWith("#")) continue;
        const parts = line.split(/\s+/);
        if (parts.length < 2) continue;
        const ip = parts[0];
        const domain = parts[1];
        if (!/^[\d.:a-fA-F]+$/.test(ip)) continue;
        // 同域名只保留一条（后行覆盖前行）
        const idx = parsed.findIndex((e) => e.domain === domain);
        if (idx >= 0) parsed[idx] = { ip, domain, managed: true };
        else parsed.push({ ip, domain, managed: true });
      }
      if (parsed.length === 0) {
        toast.info(t("tools.hostsImportEmpty"));
        return;
      }
      // 与现有托管条目按域名合并：文件里的覆盖同名域
      const merged = [
        ...entries.filter((e) => e.managed && !parsed.some((np) => np.domain === e.domain)),
        ...parsed,
      ];
      await apply(merged);
      toast.success(`${t("tools.hostsImportedP1")} ${parsed.length} ${t("tools.hostsImportedP2")}`);
    } catch (e) {
      toastError(e, t("tools.hostsWriteFailed"));
    }
  };

  const exportFile = async () => {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const path = await save({
        defaultPath: "hosts-nsb.txt",
        filters: [{ name: "hosts", extensions: ["txt", "hosts"] }],
      });
      if (!path || typeof path !== "string") return;
      const body = entries
        .filter((e) => e.managed)
        .map((e) => `${e.ip}\t${e.domain}`)
        .join("\n");
      await api.writeTextFile(path, body);
      toast.success(t("tools.hostsExported"));
    } catch (e) {
      toastError(e, t("tools.hostsWriteFailed"));
    }
  };

  return (
    <ToolCard
      icon={FilePenLine}
      title={t("tools.hosts")}
      hint={t("tools.hostsHint")}
      action={
        <div className="flex shrink-0 items-center gap-1">
          <Button size="sm" variant="ghost" onClick={importFile} title={t("tools.hostsImport")}>
            {t("tools.hostsImport")}
          </Button>
          <Button size="sm" variant="ghost" onClick={exportFile} title={t("tools.hostsExport")}>
            {t("tools.hostsExport")}
          </Button>
          <Tabs value={mode} onValueChange={(v) => (v === "text" ? openText() : setMode("list"))}>
            <TabsList>
              <TabsTrigger value="list">{t("tools.hostsModeList")}</TabsTrigger>
              <TabsTrigger value="text">{t("tools.hostsModeText")}</TabsTrigger>
            </TabsList>
          </Tabs>
        </div>
      }
    >
      {mode === "list" ? (
        <div className="flex flex-col gap-3">
          {/* 行距放宽：py-2.5 + 悬停高亮，条目多时不再挤成一坨 */}
          <div className="max-h-56 overflow-y-auto rounded-lg border border-border bg-card-2/30">
            {entries.length === 0 ? (
              <p className="px-3 py-5 text-center text-[11px] text-faint">{t("tools.hostsEmpty")}</p>
            ) : (
              entries.map((e) => (
                <div
                  key={`${e.ip}|${e.domain}`}
                  className="flex items-center gap-3 border-b border-border/50 px-3.5 py-2.5 last:border-0 hover:bg-card-2/40"
                >
                  <code className="w-28 shrink-0 font-mono text-[12px] text-faint">{e.ip}</code>
                  <code className="min-w-0 flex-1 truncate font-mono text-[12px]">{e.domain}</code>
                  {e.managed && <Badge variant="default">{t("tools.managed")}</Badge>}
                  {removable(e) && (
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      disabled={busy}
                      title={t("common.delete")}
                      className="text-faint hover:text-error"
                      onClick={() => remove(e)}
                    >
                      <Trash2 className="h-3 w-3" />
                    </Button>
                  )}
                </div>
              ))
            )}
          </div>
          <div className="flex gap-2">
            <Input
              value={newIp}
              onChange={(e) => setNewIp(e.target.value)}
              className="w-28 font-mono"
              placeholder="127.0.0.1"
            />
            <Input
              value={newDomain}
              onChange={(e) => setNewDomain(e.target.value)}
              className="flex-1 font-mono"
              placeholder="newsite.test"
              onKeyDown={(e) => e.key === "Enter" && add()}
            />
            <Button variant="secondary" disabled={busy || !newDomain} onClick={add}>
              {t("tools.add")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <p className="text-[10.5px] text-faint">{t("tools.hostsTextHint")}</p>
          <textarea
            value={textDraft}
            onChange={(e) => setTextDraft(e.target.value)}
            rows={10}
            spellCheck={false}
            style={paletteVars}
            className="nsb-code w-full rounded-lg border border-border p-3 font-mono text-[11.5px] leading-relaxed outline-none focus:border-border-strong"
            placeholder={"127.0.0.1  newsite.test\n10.0.0.9  nas.test"}
          />
          <div className="flex gap-2">
            <Button variant="secondary" size="sm" disabled={busy} onClick={applyText}>
              {t("tools.hostsApplyText")}
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setMode("list")}>
              {t("common.cancel")}
            </Button>
          </div>
        </div>
      )}
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
  const invalidate = useInvalidate();
  const { data: services = [] } = useServices(3000);
  const dns = services.find((sv) => sv.id.startsWith("coredns"));
  const [busy, setBusy] = React.useState(false);
  const running = dns?.state === "running";
  const port = dns?.port ?? 53;

  const toggle = async (next: boolean) => {
    if (!dns) {
      toast.info(t("dns.notInstalled"));
      return;
    }
    setBusy(true);
    try {
      if (next) await api.startService(dns.id);
      else await api.stopService(dns.id);
      invalidate("services");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  /** 一键接管：把已连接网卡的 DNS 指向 127.0.0.1（触发 UAC） */
  const [interfaces, setInterfaces] = React.useState<string[]>([]);
  const [activeIf, setActiveIf] = React.useState<string | null>(null);
  const [takeoverBusy, setTakeoverBusy] = React.useState(false);

  const loadInterfaces = React.useCallback(async () => {
    try {
      const list = await api.dnsInterfaces();
      setInterfaces(list);
      if (list.length > 0) setActiveIf((cur) => cur ?? list[0]);
    } catch {
      /* dev 模式 mock 未实现 */
    }
  }, []);

  React.useEffect(() => {
    loadInterfaces();
  }, [loadInterfaces]);

  const takeover = async () => {
    if (!activeIf) return;
    setTakeoverBusy(true);
    try {
      await api.dnsTakeover(activeIf);
      toast.success(`${t("dns.takeoverDoneP1")} ${activeIf} ${t("dns.takeoverDoneP2")}`);
    } catch (e) {
      toastError(e);
    } finally {
      setTakeoverBusy(false);
    }
  };

  const restore = async () => {
    if (!activeIf) return;
    setTakeoverBusy(true);
    try {
      await api.dnsRestore(activeIf);
      toast.success(t("dns.restoreDone"));
    } catch (e) {
      toastError(e);
    } finally {
      setTakeoverBusy(false);
    }
  };

  return (
    <ToolCard icon={Radar} title={t("dns.title")} hint={t("dns.hint")}>
      <div className="flex flex-col gap-3">
        <div className="flex items-center justify-between gap-3">
          <div>
            <p className="text-[12.5px] font-medium">
              {running ? t("dns.running") : t("dns.stopped")}
              <span className="ml-2 font-mono text-[11px] text-faint">:{port}</span>
            </p>
            <p className="mt-0.5 text-[10.5px] text-faint">{t("dns.stateHint")}</p>
          </div>
          <Button
            variant={running ? "secondary" : "secondary"}
            size="sm"
            disabled={busy || !dns}
            onClick={() => toggle(!running)}
          >
            {running ? t("dns.stop") : t("dns.start")}
          </Button>
        </div>

        {/* 一键接管（UAC） */}
        <div className="rounded-lg border border-border bg-card-2/30 p-2.5">
          <div className="flex flex-col items-start justify-between gap-2 sm:flex-row sm:items-center">
            <div className="min-w-0">
              <p className="text-[11.5px] font-medium text-secondary">{t("dns.takeoverTitle")}</p>
              <p className="mt-0.5 text-[10.5px] text-faint">{t("dns.takeoverHint")}</p>
            </div>
            <div className="flex w-full shrink-0 flex-wrap gap-1.5 sm:w-auto">
              {interfaces.length > 1 && (
                <Select
                  value={activeIf ?? ""}
                  onValueChange={setActiveIf}
                  disabled={takeoverBusy}
                >
                  <SelectTrigger aria-label={t("dns.interface")} className="h-7 max-w-32 px-2 text-[11px]">
                    <SelectValue placeholder={t("dns.interface")} />
                  </SelectTrigger>
                  <SelectContent>
                    {interfaces.map((n) => <SelectItem key={n} value={n}>{n}</SelectItem>)}
                  </SelectContent>
                </Select>
              )}
              <Button size="sm" variant="secondary" disabled={takeoverBusy || !activeIf} onClick={takeover}>
                {t("dns.takeover")}
              </Button>
              <Button size="sm" variant="ghost" disabled={takeoverBusy || !activeIf} onClick={restore}>
                {t("dns.restore")}
              </Button>
            </div>
          </div>
        </div>

        <div className="rounded-lg border border-border bg-card-2/30 p-2.5 text-[11px] leading-relaxed text-muted">
          <p className="font-medium text-secondary">{t("dns.setupTitle")}</p>
          <ol className="mt-1 list-decimal space-y-0.5 pl-4">
            <li>{t("dns.setupStep1")}</li>
            <li>
              {t("dns.setupStep2")}
              <code className="ml-1 rounded bg-card px-1 py-px font-mono text-[10.5px]">
                nslookup anything.test 127.0.0.1
              </code>
            </li>
          </ol>
          <p className="mt-1.5 text-[10.5px] text-faint">{t("dns.setupNote")}</p>
        </div>
      </div>
    </ToolCard>
  );
}
