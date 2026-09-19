"use client";

import * as React from "react";
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
} from "lucide-react";
import type { ListenerInfo, PortDiagnosis, PortScanEntry } from "@nsb/schema";
import { useUI, useT } from "@/lib/store";
import { useHosts, useInvalidate, toastError, useSettings } from "@/lib/hooks";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { CopyButton, ConfirmDialog } from "@/components/shared/misc";
import { CodeBlock } from "@/components/shared/code-block";
import { PathEnvCard } from "@/components/shared/path-env-card";

const COMMON_PORTS = [80, 443, 8080, 8443, 3306, 23306, 6379, 26379, 9000, 5432];

export default function ToolsPage() {
  const t = useT();
  return (
    <div className="pb-8">
      <PageHeaderInline title={t("tools.title")} subtitle={t("tools.subtitle")} />
      <div className="grid grid-cols-1 gap-5 xl:grid-cols-2">
        <PortLookupTool />
        <HostsTool />
        <PortTool />
        <PathEnvCard />
        <TerminalInjectTool />
        <RewriteTemplates />
        <BackupTool />
        <RepairTool />
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
  children,
}: {
  icon: React.ComponentType<{ className?: string; strokeWidth?: number }>;
  title: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <Card>
      <CardHeader className="flex-row items-center gap-3">
        <div className="flex h-9 w-9 items-center justify-center rounded-lg border border-border bg-card-2/60">
          <Icon className="h-4 w-4 text-primary" strokeWidth={1.8} />
        </div>
        <div>
          <CardTitle className="text-[13px]">{title}</CardTitle>
          <CardDescription className="text-[11px]">{hint}</CardDescription>
        </div>
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

/* ============ hosts 编辑 ============ */
function HostsTool() {
  const t = useT();
  const { data: entries } = useHosts();
  const invalidate = useInvalidate();
  const [newDomain, setNewDomain] = React.useState("");
  const [newIp, setNewIp] = React.useState("127.0.0.1");
  const [busy, setBusy] = React.useState(false);

  return (
    <ToolCard icon={FilePenLine} title={t("tools.hosts")} hint={t("tools.hostsHint")}>
      <div className="flex flex-col gap-3">
        <div className="max-h-44 overflow-y-auto rounded-lg border border-border bg-card-2/30">
          {entries.length === 0 ? (
            <p className="px-3 py-4 text-center text-[11px] text-faint">{t("tools.hostsEmpty")}</p>
          ) : (
            entries.map((e) => (
              <div key={`${e.ip}|${e.domain}`} className="flex items-center gap-2 border-b border-border/60 px-3 py-2 last:border-0">
                <code className="w-24 shrink-0 font-mono text-[11.5px] text-faint">{e.ip}</code>
                <code className="flex-1 truncate font-mono text-[11.5px]">{e.domain}</code>
                {e.managed && <Badge variant="default">{t("tools.managed")}</Badge>}
              </div>
            ))
          )}
        </div>
        <div className="flex gap-2">
          <Input value={newIp} onChange={(e) => setNewIp(e.target.value)} className="w-28 font-mono" placeholder="127.0.0.1" />
          <Input
            value={newDomain}
            onChange={(e) => setNewDomain(e.target.value)}
            className="flex-1 font-mono"
            placeholder="newsite.test"
          />
          <Button
            variant="secondary"
            disabled={busy || !newDomain}
            onClick={async () => {
              setBusy(true);
              try {
                await api.applyHosts([...entries, { ip: newIp, domain: newDomain, managed: true }]);
                toast.success(`${t("tools.hostsUpdatedP1")}${newDomain}`);
                setNewDomain("");
                invalidate("hosts");
              } catch (e) {
                toastError(e, t("tools.hostsWriteFailed"));
              } finally {
                setBusy(false);
              }
            }}
          >
            {t("tools.add")}
          </Button>
        </div>
      </div>
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
                className="rounded-md border border-border bg-card-2/40 px-1.5 py-0.5 font-mono text-[10.5px] text-faint transition-colors hover:border-border-strong hover:text-secondary"
              >
                {p}
              </button>
            ))}
          </div>

          <div className="max-h-64 overflow-y-auto rounded-lg border border-border bg-card-2/30">
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
        <div className="max-h-56 overflow-y-auto rounded-lg border border-border bg-card-2/30">
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
          <div className="max-h-40 overflow-y-auto rounded-lg border border-border bg-card-2/30">
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
          setScript(isWin ? "# 还没有已安装的运行时" : "# 还没有已安装的运行时");
          return;
        }
        const checks = [php ? "php -v" : "", mysql ? "mysql --version" : ""].filter(Boolean).join("; ");
        const header = isWin
          ? "# PowerShell - 将本应用运行时注入当前会话 PATH（不污染系统）"
          : "# zsh / bash - 将本应用运行时注入当前会话 PATH（不污染系统）";
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
  ThinkPHP: `location / {
    if (!-e $request_filename) {
        rewrite ^(.*)$ /index.php?s=$1 last;
    }
}`,
  WordPress: `location / {
    try_files $uri $uri/ /index.php?$args;
}
rewrite /wp-admin$ $scheme://$host$uri/ permanent;`,
  "SPA fallback": `location / {
    try_files $uri $uri/ /index.html;
}`,
};

function RewriteTemplates() {
  const t = useT();
  return (
    <ToolCard icon={FileCode2} title={t("tools.rewriteTemplates")} hint="nginx location 预设，创建站点时可直接选择">
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
  const [busy, setBusy] = React.useState(false);
  const [restoring, setRestoring] = React.useState<string | null>(null);
  const [backups, setBackups] = React.useState<api.BackupFile[]>([]);
  const [dataDir, setDataDir] = React.useState("");
  const [restoreTarget, setRestoreTarget] = React.useState<string | null>(null);

  const load = React.useCallback(async () => {
    setBusy(true);
    try {
      const [list, dir] = await Promise.all([
        api.listBackups().catch(() => [] as api.BackupFile[]),
        api.getDataDir().catch(() => ""),
      ]);
      setBackups(list);
      setDataDir(dir);
    } finally {
      setBusy(false);
    }
  }, []);

  React.useEffect(() => {
    load();
  }, [load]);

  return (
    <ToolCard icon={DatabaseBackup} title={t("tools.backup")} hint={t("tools.backupHint")}>
      <div className="flex flex-col gap-3">
        <div className="max-h-44 overflow-y-auto rounded-lg border border-border bg-card-2/30">
          {backups.length === 0 ? (
            <p className="px-3 py-4 text-center text-[11px] text-faint">
              {busy ? t("common.loading") : t("tools.noBackups")}
            </p>
          ) : (
            backups.map((b) => (
              <div key={b.name} className="flex items-center gap-2 border-b border-border/60 px-3 py-2 last:border-0">
                <code className="flex-1 truncate font-mono text-[11.5px]">{b.name}</code>
                <span className="shrink-0 text-[10.5px] text-faint">{formatBytes(b.sizeBytes)}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={restoring === b.name}
                  onClick={() => setRestoreTarget(b.name)}
                >
                  {restoring === b.name ? <Loader2 className="h-3 w-3 animate-spin" /> : null}
                  {t("tools.restore")}
                </Button>
              </div>
            ))
          )}
        </div>
        <div className="flex gap-2">
          <Button variant="secondary" size="sm" onClick={load} disabled={busy}>
            {t("tools.refresh")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              if (!dataDir) return;
              api
                .openInFolder(`${dataDir}/backup`)
                .then(() => toast.success(t("tools.backupOpened")))
                .catch(toastError);
            }}
          >
            {t("tools.openBackupDirBtn")}
          </Button>
        </div>
      </div>

      <ConfirmDialog
        open={restoreTarget !== null}
        onOpenChange={(o) => !o && setRestoreTarget(null)}
        title={t("confirm.restoreBackup")}
        description={`${t("confirm.restoreBackupDesc")}\n\n${restoreTarget ?? ""}`}
        confirmText={t("tools.restore")}
        danger
        loading={restoring !== null}
        onConfirm={async () => {
          if (!restoreTarget) return;
          setRestoring(restoreTarget);
          try {
            await api.restoreBackup(restoreTarget);
            toast.success(t("tools.restored"));
            load();
          } catch (e) {
            toastError(e);
          } finally {
            setRestoring(null);
            setRestoreTarget(null);
          }
        }}
      />
    </ToolCard>
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
  const items = [
    {
      id: "configs",
      label: t("tools.rebuildConf"),
      desc: t("tools.rebuildConfHint"),
      action: async () => {
        await api.restartService("nginx");
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
        {items.map((item) => (
          <div key={item.id} className="flex items-center justify-between gap-3 rounded-lg border border-border bg-card-2/30 p-3">
            <div>
              <p className="text-[12px] font-medium">{item.label}</p>
              <p className="text-[10.5px] text-faint">{item.desc}</p>
            </div>
            <Button
              size="sm"
              variant="secondary"
              disabled={busy === item.id}
              onClick={async () => {
                setBusy(item.id);
                try {
                  await item.action();
                  toast.success(`${item.label} ${t("tools.wizardDoneP2")}`);
                  invalidate("services", "hosts", "certs");
                } catch (e) {
                  toastError(e);
                } finally {
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
    </ToolCard>
  );
}
