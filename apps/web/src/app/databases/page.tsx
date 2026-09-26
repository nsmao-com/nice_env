"use client";

import * as React from "react";
import { toast } from "sonner";
import { Database, HardDrive, KeyRound, Play, Plus, Table2, Trash2, UserRound, ExternalLink, Loader2, Import } from "lucide-react";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { useDatabases, useDbUsers, useInvalidate, toastError, useAdminer, useServices } from "@/lib/hooks";
import { useQuery } from "@tanstack/react-query";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { DbBackupCard } from "@/components/shared/db-backup";
import { DbImportDialog } from "@/components/shared/db-import-dialog";
import { ServiceIcon } from "@/components/shared/service-icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/misc";
import { CopyButton, SectionHeader, ConfirmDialog } from "@/components/shared/misc";
import { StatChip } from "@/components/shared/stat-chip";
import { StatusLight } from "@/components/shared/status-light";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from "@/components/ui/dialog";
import type { ServiceStatus } from "@nsb/schema";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { PageHeader } from "@/components/layout/app-shell";

export default function DatabasesPage() {
  const t = useT();
  const invalidate = useInvalidate();
  const serviceQuery = useServices();
  const mysqlServices = serviceQuery.data.filter((s) => s.id === "mysql" || s.id.startsWith("mysql@"));
  const [selectedVersion, setSelectedVersion] = React.useState("");
  const version = selectedVersion || mysqlServices.find((s) => s.state === "running")?.version || mysqlServices[0]?.version || "";
  const service = mysqlServices.find((s) => s.version === version);
  const running = !!version && service?.state === "running";
  const dbQuery = useDatabases(version, running);
  const userQuery = useDbUsers(version, running);
  const dbs = dbQuery.data ?? [];
  const users = userQuery.data ?? [];
  const ready = running && dbQuery.isSuccess;
  const targetLabel = `MySQL ${version} · 127.0.0.1:${service?.port ?? "—"}`;
  const [backupLocked, setBackupLocked] = React.useState(false);
  const [createOpen, setCreateOpen] = React.useState(false);
  const [userOpen, setUserOpen] = React.useState(false);
  const [rootOpen, setRootOpen] = React.useState(false);
  const [importOpen, setImportOpen] = React.useState(false);
  const [dropTarget, setDropTarget] = React.useState<string | null>(null);
  const [dropping, setDropping] = React.useState(false);
  const [dropTyped, setDropTyped] = React.useState("");

  const systemDbs = new Set(["mysql", "sys", "information_schema", "performance_schema"]);

  return (
    <div className="pb-8">
      <DbImportDialog key={`import-${version}`} open={importOpen} onOpenChange={setImportOpen} version={version} targetLabel={targetLabel} />
      <PageHeader
        title={t("db.title")}
        subtitle={t("db.subtitle")}
        actions={
          <>
            <Button variant="secondary" disabled={!running} onClick={() => setRootOpen(true)}>
              <KeyRound className="h-3.5 w-3.5" /> {t("db.rootManage")}
            </Button>
            <Button variant="secondary" disabled={!ready} onClick={() => setImportOpen(true)}>
              <Import className="h-3.5 w-3.5" /> {t("dbImport.open")}
            </Button>
            <Button disabled={!ready} onClick={() => setCreateOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("db.createDb")}
            </Button>
          </>
        }
      />

      <div className="mb-5 flex flex-wrap items-end gap-3">
        <div className="w-full max-w-sm space-y-1.5">
          <Label htmlFor="mysql-instance">{t("db.chooseInstance")}</Label>
          <Select value={version} onValueChange={setSelectedVersion} disabled={createOpen || userOpen || rootOpen || importOpen || !!dropTarget || backupLocked || !mysqlServices.length}>
            <SelectTrigger id="mysql-instance"><SelectValue placeholder={t("db.chooseInstance")} /></SelectTrigger>
            <SelectContent>{mysqlServices.map((s) => <SelectItem key={s.id} value={s.version!}>MySQL {s.version} · {s.port ?? "—"} · {s.state === "running" ? t("common.running") : t("common.stopped")}</SelectItem>)}</SelectContent>
          </Select>
        </div>
        <Button variant="secondary" disabled={!running || dbQuery.isFetching || userQuery.isFetching} onClick={() => invalidate("databases", "db-users")}>{t("db.refresh")}</Button>
      </div>
      {serviceQuery.isError ? <p role="alert" className="mb-4 text-sm text-error">{t("db.connectionFailed")} <Button variant="ghost" onClick={() => void serviceQuery.refetch()}>{t("db.retry")}</Button></p> : !mysqlServices.length ? <p className="mb-4 text-sm text-muted">{serviceQuery.isFetching ? t("db.loading") : t("db.noInstance")}</p> : null}
      {/* 实例卡片：MySQL + Redis */}
      <section className="mb-6">
        <SectionHeader title={t("db.instance")} className="mb-3" />
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          <MySqlInstanceCard key={version} service={service} count={ready ? dbs.length : undefined} />
          <RedisInstanceCard />
        </div>
      </section>

      {/* 备份 / 还原 */}
      <section className="mb-6">
        <DbBackupCard key={version} version={version} targetLabel={targetLabel} ready={ready} databases={dbs.filter((db) => !systemDbs.has(db.name.toLowerCase())).map((db) => db.name)} onLockChange={setBackupLocked} />
      </section>

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-3">
        {/* 数据库列表 */}
        <Card className="lg:col-span-2">
          <CardHeader className="flex-row items-center justify-between">
            <CardTitle className="flex items-center gap-2 text-[13px]">
              <Database className="h-3.5 w-3.5 text-primary" /> {t("db.databases")}
            </CardTitle>
            <Button variant="ghost" size="sm" disabled={!ready || !dbs.some((db) => !systemDbs.has(db.name.toLowerCase()))} onClick={() => setUserOpen(true)}>
              <UserRound className="h-3.5 w-3.5" /> {t("db.createUser")}
            </Button>
          </CardHeader>
          <CardContent>
            {!running || dbQuery.isPending || dbQuery.isError ? (
              <p role={dbQuery.isError ? "alert" : "status"} className="py-6 text-sm text-muted">{!running ? t("db.serviceStoppedHint") : dbQuery.isError ? t("db.connectionFailed") : t("db.loading")}</p>
            ) : dbs.length === 0 ? (
              <p className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-xs text-faint">
                {t("db.emptyHint")}
              </p>
            ) : (
              <div className="flex flex-col divide-y divide-border">
                {dbs.map((db) => (
                  <div key={db.name} className="flex flex-wrap items-center gap-2 py-2.5 sm:gap-3">
                    <Table2 className="h-3.5 w-3.5 shrink-0 text-faint" />
                    <span className="flex-1 truncate font-mono text-[12.5px]">{db.name}</span>
                    {db.tables != null && <StatChip icon={Table2}>{db.tables} {t("db.tables")}</StatChip>}
                    {db.sizeKb != null && db.sizeKb > 0 && (
                      <StatChip icon={HardDrive}>{fmtBytes(db.sizeKb * 1024)}</StatChip>
                    )}
                    {systemDbs.has(db.name.toLowerCase()) && <Badge variant="muted">{t("db.systemDb")}</Badge>}
                    <CopyButton text={`mysql://root@127.0.0.1:${service?.port}/${encodeURIComponent(db.name)}`} />
                    {/* 系统库不给删：删了 MySQL 直接起不来 */}
                    {!systemDbs.has(db.name.toLowerCase()) && (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        title={t("db.drop")}
                        aria-label={`${t("db.drop")} ${db.name}`}
                        disabled={!ready}
                        className="text-faint hover:text-error"
                        onClick={() => {
                          setDropTarget(db.name);
                          setDropTyped("");
                        }}
                      >
                        <Trash2 className="h-3 w-3" />
                      </Button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>

        {/* 账号 */}
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-[13px]">
              <UserRound className="h-3.5 w-3.5 text-primary" /> {t("db.users")}
            </CardTitle>
          </CardHeader>
          <CardContent className="flex flex-col gap-2">
            {!running || userQuery.isPending || userQuery.isError ? (
              <p role={userQuery.isError ? "alert" : "status"} className="text-sm text-muted">{!running ? t("db.serviceStoppedHint") : userQuery.isError ? t("db.connectionFailed") : t("db.loading")}</p>
            ) : users.length === 0 ? (
              <p className="text-xs text-faint">{t("db.noUsers")}</p>
            ) : (
              users.map((u) => (
                <div key={`${u.username}@${u.host}`} className="rounded-md bg-fill px-3 py-2">
                  <div className="flex items-center justify-between">
                    <span className="font-mono text-[12px]">{u.username}</span>
                    <span className="text-[10px] text-faint">@{u.host}</span>
                  </div>
                  {u.grants && <p className="mt-0.5 text-[10.5px] text-faint">{u.grants}</p>}
                </div>
              ))
            )}
          </CardContent>
        </Card>
      </div>

      <CreateDbDialog key={`create-db-${version}`} version={version} targetLabel={targetLabel} open={createOpen} onOpenChange={setCreateOpen} onDone={() => invalidate("databases")} />
      <CreateUserDialog key={`create-user-${version}`} version={version} targetLabel={targetLabel} open={userOpen} onOpenChange={setUserOpen} onDone={() => invalidate("db-users")} />
      <ResetRootDialog key={`root-${version}`} version={version} targetLabel={targetLabel} open={rootOpen} onOpenChange={setRootOpen} />

      {/* 删库不可恢复：要求用户把库名完整敲一遍才允许执行 */}
      <ConfirmDialog
        open={dropTarget !== null}
        onOpenChange={(o) => !o && !dropping && setDropTarget(null)}
        title={`${t("confirm.deleteDb")} · ${dropTarget ?? ""}`}
        description={`${targetLabel} · ${t("confirm.deleteDbDesc").replace("{name}", dropTarget ?? "")}`}
        confirmText={t("db.drop")}
        danger
        loading={dropping}
        confirmDisabled={!ready || !dropTarget || dropTyped !== dropTarget}
        onConfirm={async () => {
          if (dropping || !ready || !dropTarget || dropTyped !== dropTarget) return;
          setDropping(true);
          try {
            await api.dbDrop(dropTarget, version);
            setDropTarget(null);
            toast.success(`${t("db.droppedP1")} ${dropTarget} ${t("db.droppedP2")}`);
            invalidate("databases");
          } catch (e) {
            toastError(e);
          } finally {
            setDropping(false);
          }
        }}
      >
        <div className="flex flex-col gap-2">
          <Label className="text-[11.5px] text-faint">
            {t("confirm.deleteDbPrompt").replace("{name}", dropTarget ?? "")}
          </Label>
          <Input
            aria-label={t("confirm.deleteDbPrompt").replace("{name}", dropTarget ?? "")}
            disabled={dropping}
            value={dropTyped}
            onChange={(e) => setDropTyped(e.target.value)}
            placeholder={dropTarget ?? ""}
            className="font-mono text-[12px]"
          />
          {dropTyped.length > 0 && dropTyped !== dropTarget && (
            <p className="text-[10.5px] text-error">{t("confirm.mismatch")}</p>
          )}
        </div>
      </ConfirmDialog>
    </div>
  );
}

/** 实例卡对应的服务状态（栈 id 可能带版本后缀：mysql / mysql@8.0.46） */
function useInstanceState(base: string, version?: string) {
  const { data: services } = useServices(3000);
  return React.useMemo(
    () =>
      (version ? services.find((s) => s.version === version && (s.id === base || s.id.startsWith(`${base}@`))) : undefined) ??
      (!version ? services.find((s) => s.id === base) ??
      services.find((s) => s.id.startsWith(`${base}@`)) : undefined) ?? null,
    [services, base, version]
  );
}

/** 服务没跑就一键拉起：修数据库的人不用再跳回总览找开关 */
function InstanceStartButton({ base, version }: { base: string; version?: string }) {
  const t = useT();
  const invalidate = useInvalidate();
  const svc = useInstanceState(base, version);
  const [busy, setBusy] = React.useState(false);
  if (!svc) return null;
  if (svc.state === "running") {
    return <StatusLight state={svc.state} size={8} />;
  }
  const start = async () => {
    setBusy(true);
    try {
      await api.startService(svc.id);
      toast.success(`${svc.label} · ${t("common.running")}`);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
      invalidate("services");
    }
  };
  return (
    <Button
      size="sm"
      variant="secondary"
      disabled={busy || svc.state === "starting" || svc.state === "stopping"}
      title={t("db.serviceStoppedHint")}
      onClick={start}
    >
      {busy || svc.state === "starting" ? (
        <Loader2 className="h-3 w-3 animate-spin" />
      ) : (
        <Play className="h-3 w-3" />
      )}
      {t("common.start")}
    </Button>
  );
}

function MySqlInstanceCard({ service, count }: { service?: ServiceStatus; count?: number }) {
  const t = useT();
  const adminer = useAdminer();
  const port = service?.port;
  return (
    <Card className="p-4">
      <div className="mb-3 flex items-center gap-2">
        <div className="flex h-9 w-9 items-center justify-center rounded-md bg-fill">
          <ServiceIcon id="mysql" className="h-[18px] w-[18px]" />
        </div>
        <div>
          <p className="text-[13px] font-medium">MySQL {service?.version}</p>
          <p className="text-[11px] text-faint">127.0.0.1:{port ?? "—"} · utf8mb4</p>
        </div>
        <div className="ml-auto">
          <InstanceStartButton base="mysql" version={service?.version} />
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-1.5">
        <StatChip icon={Database}>{count ?? "—"} {t("db.dbs")}</StatChip>
        <StatChip icon={HardDrive}>{t("db.datadir")} · {t("db.datadirSuffix")}</StatChip>
      </div>
      <Separator className="my-3" />
      <div className="flex flex-col gap-1.5">
        <p className="text-[11px] font-medium text-secondary">{t("db.connStrings")}</p>
        {[
          ["URL", `mysql://root@127.0.0.1:${port ?? "—"}/dbname`],
          ["CLI", `mysql -u root -p -h 127.0.0.1 -P ${port ?? "—"}`],
          ["PDO", `"mysql:host=127.0.0.1;port=${port ?? "—"};dbname=dbname"`],
        ].map(([k, v]) => (
          <div key={k} className="flex items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5">
            <span className="w-7 shrink-0 text-[10px] font-medium text-faint">{k}</span>
            <code className="min-w-0 flex-1 truncate font-mono text-[11px] text-secondary">{v}</code>
            <CopyButton text={v} />
          </div>
        ))}
      </div>
      <div className="mt-3 flex gap-2">
        <Button variant="secondary" size="sm" disabled={adminer.busy} onClick={() => void adminer.open()}>
          {adminer.busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <ExternalLink className="h-3.5 w-3.5" />} {t("db.openAdminer")}
        </Button>
      </div>
    </Card>
  );
}

function RedisInstanceCard() {
  const t = useT();
  const service = useInstanceState("redis");
  const running = service?.state === "running" || (service?.state === "error" && service.pids.length > 0);
  const [connectionTarget, setConnectionTarget] = React.useState<ServiceStatus | null>(null);
  const query = useQuery({
    queryKey: ["redis-stats", service?.version, service?.port, service?.pids.join(",")],
    queryFn: api.redisStats, enabled: running, refetchInterval: running ? 5000 : false, retry: false,
  });
  const rs = running && !query.isError ? query.data : undefined;
  const port = service?.port;
  const error = query.error ? normalizeError(query.error) : null;
  return (
    <>
    <Card className="min-w-0 p-4">
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill"><ServiceIcon id="redis" className="h-[18px] w-[18px]" /></div>
        <div className="min-w-0 flex-1">
          <p className="text-[13px] font-medium">Redis {service?.version}</p>
          <p className="text-[11px] text-faint">127.0.0.1:{port ?? "—"}</p>
        </div>
        <InstanceStartButton base="redis" />
      </div>
      {!running ? <p className="text-xs text-muted">{t("db.redisStopped")}</p> : <>
        {query.isPending && <p role="status" className="mb-3 text-xs text-muted">{t("db.redisLoading")}</p>}
        {error && <div role="alert" className="mb-3 space-y-1 rounded-md bg-warning-soft p-2.5 text-xs text-muted">
          <p className="break-words">{error.message}</p>{error.hint && <p className="break-words">{error.hint}</p>}
          <Button size="sm" variant="ghost" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("db.retry")}</Button>
        </div>}
        {rs && <p className="mb-3 text-xs text-success">{rs.usedMemoryHuman ?? "—"} · {rs.keys ?? "—"} keys · {rs.connectedClients ?? "—"} {t("db.redisClients")}</p>}
        <Button variant="secondary" size="sm" className="mb-3" onClick={() => service && setConnectionTarget(service)}><KeyRound className="h-3.5 w-3.5" />{t("db.redisAuth")}</Button>
        <div className="flex flex-col gap-1.5">
          <p className="text-[11px] font-medium text-secondary">{t("db.connMethod")}</p>
          {[["CLI", `redis-cli -h 127.0.0.1 -p ${port}`], ["URL", `redis://127.0.0.1:${port}/0`]].map(([label, value]) => (
            <div key={label} className="flex min-w-0 items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5">
              <span className="w-7 shrink-0 text-[10px] font-medium text-faint">{label}</span>
              <code className="min-w-0 flex-1 truncate font-mono text-[11px] text-secondary">{value}</code><CopyButton text={value} />
            </div>
          ))}
        </div>
      </>}
    </Card>
    {connectionTarget?.version && <RedisConnectionDialog key={connectionTarget.version} service={connectionTarget} open onOpenChange={(open) => !open && setConnectionTarget(null)} />}
    </>
  );
}

function RedisConnectionDialog({ service, open, onOpenChange }: { service: ServiceStatus; open: boolean; onOpenChange: (open: boolean) => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const version = service.version!;
  const query = useQuery({ queryKey: ["redis-connection", version], queryFn: () => api.redisConnection(version), retry: false });
  const [mode, setMode] = React.useState("password");
  const [username, setUsername] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const busyRef = React.useRef(false);
  const edited = React.useRef(false);
  const [error, setError] = React.useState("");
  React.useEffect(() => { if (query.data && !edited.current) setUsername(query.data.username); }, [query.data]);
  const valid = mode === "none" || (password.length > 0 && !/[\x00]/.test(password + username));
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (busyRef.current || !valid) return;
    busyRef.current = true; setBusy(true); setError("");
    try {
      await api.redisSaveConnection(version, mode === "none" ? { username: "", password: "" } : { username, password });
      setPassword(""); toast.success(t("db.redisAuthSaved"));
      invalidate("redis-stats", "redis-connection", "services"); onOpenChange(false);
    } catch (error) { setError(normalizeError(error).message); }
    finally { busyRef.current = false; setBusy(false); }
  };
  return <Dialog open={open} onOpenChange={(value) => !busyRef.current && onOpenChange(value)}>
    <DialogContent hideClose={busy} className="flex max-w-md max-h-[85dvh] flex-col overflow-hidden">
      <DialogHeader><DialogTitle>{t("db.redisAuth")}</DialogTitle><DialogDescription>Redis {version} · 127.0.0.1:{service.port}</DialogDescription></DialogHeader>
      <form className="flex min-h-0 flex-col gap-4" onSubmit={submit}>
        <div className="min-h-0 space-y-4 overflow-y-auto">
          <p className="text-xs text-muted">{t("db.redisAuthHint")}</p>
          {query.isPending && <p role="status" className="text-xs text-muted">{t("common.loading")}</p>}
          {query.isError && <p role="alert" className="text-xs text-error">{t("db.redisAuthLoadFailed")} <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void query.refetch()}>{t("db.retry")}</Button></p>}
          {query.data?.hasPassword && <p className="text-xs text-muted">{t("db.redisPasswordSaved")}</p>}
          <div className="space-y-1.5"><Label htmlFor="redis-auth-mode">{t("db.operation")}</Label><Select value={mode} disabled={busy} onValueChange={(value) => { setMode(value); setPassword(""); setError(""); }}><SelectTrigger id="redis-auth-mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="password">{t("db.redisWithPassword")}</SelectItem><SelectItem value="none">{t("db.redisWithoutPassword")}</SelectItem></SelectContent></Select></div>
          {mode === "password" && <>
            <div className="space-y-1.5"><Label htmlFor="redis-auth-user">{t("db.redisAclUser")}</Label><Input id="redis-auth-user" value={username} maxLength={512} disabled={busy} placeholder="default" autoComplete="username" onChange={(event) => { edited.current = true; setUsername(event.target.value); }} /></div>
            <div className="space-y-1.5"><Label htmlFor="redis-auth-password">{t("db.password")}</Label><Input id="redis-auth-password" type="password" value={password} disabled={busy} maxLength={16384} autoComplete="current-password" onChange={(event) => setPassword(event.target.value)} /></div>
          </>}
          {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
        </div>
        <DialogFooter className="shrink-0"><Button type="button" variant="ghost" disabled={busy} onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !valid}>{busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}{t("db.redisVerifySave")}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}

type DatabaseDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  version: string;
  targetLabel: string;
  onDone?: () => void;
};

function CreateDbDialog({ open, onOpenChange, onDone, version, targetLabel }: DatabaseDialogProps) {
  const t = useT();
  const [name, setName] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const valid = /^[A-Za-z0-9_]{1,64}$/.test(name);
  const submit = async () => {
    if (busy || !valid || !version) return;
    setBusy(true);
    try {
      await api.dbCreate(name, version);
      toast.success(`${t("db.createdP1")} ${name} ${t("db.createdP2")}`);
      onOpenChange(false); setName(""); onDone?.();
    } catch (error) { toastError(error); } finally { setBusy(false); }
  };
  return <Dialog open={open} onOpenChange={(value) => !busy && onOpenChange(value)}>
    <DialogContent className="max-w-md max-h-[85dvh] overflow-y-auto" hideClose={busy}>
      <DialogHeader><DialogTitle>{t("db.createDb")}</DialogTitle><DialogDescription>{targetLabel}</DialogDescription></DialogHeader>
      <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <div className="space-y-1.5"><Label htmlFor="new-db-name">{t("db.databases")}</Label><Input id="new-db-name" disabled={busy} value={name} onChange={(e) => setName(e.target.value)} placeholder="my_database" maxLength={64} /></div>
        <p className="text-xs text-muted">{t("db.identifierHint")}</p>
        <DialogFooter><Button type="button" variant="ghost" disabled={busy} onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button><Button type="submit" disabled={!valid || busy}>{busy ? t("confirm.busy") : t("db.create")}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}

function CreateUserDialog({ open, onOpenChange, onDone, version, targetLabel }: DatabaseDialogProps) {
  const t = useT();
  const [form, setForm] = React.useState({ username: "", password: "", database: "" });
  const [busy, setBusy] = React.useState(false);
  const query = useDatabases(version, open && !!version);
  const dbs = (query.data ?? []).filter((db) => !["mysql", "sys", "information_schema", "performance_schema"].includes(db.name.toLowerCase()));
  const valid = /^[A-Za-z0-9_]{1,32}$/.test(form.username) && form.username.toLowerCase() !== "root" && !!form.password && !/[\x00-\x1f\x7f]/.test(form.password) && dbs.some((db) => db.name === form.database);
  const submit = async () => {
    if (busy || !valid || query.isError) return;
    setBusy(true);
    try {
      await api.dbCreateUser(form.username, form.password, form.database, version);
      toast.success(`${t("db.userCreatedP1")} ${form.username} ${t("db.userCreatedP2")}`);
      onOpenChange(false); setForm({ username: "", password: "", database: "" }); onDone?.();
    } catch (error) { toastError(error); } finally { setBusy(false); }
  };
  return <Dialog open={open} onOpenChange={(value) => !busy && onOpenChange(value)}>
    <DialogContent className="max-w-md max-h-[85dvh] overflow-y-auto" hideClose={busy}>
      <DialogHeader><DialogTitle>{t("db.createUser")}</DialogTitle><DialogDescription>{targetLabel} · {t("db.grantHint")}</DialogDescription></DialogHeader>
      <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <div className="space-y-1.5"><Label htmlFor="new-db-user">{t("db.username")}</Label><Input id="new-db-user" disabled={busy} value={form.username} maxLength={32} onChange={(e) => setForm({ ...form, username: e.target.value })} autoComplete="off" /></div>
        <div className="space-y-1.5"><Label htmlFor="new-db-password">{t("db.password")}</Label><Input id="new-db-password" type="password" disabled={busy} value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} autoComplete="new-password" /></div>
        <div className="space-y-1.5"><Label htmlFor="grant-database">{t("db.grantDb")}</Label><Select value={form.database} disabled={busy || query.isError || query.isPending} onValueChange={(database) => setForm({ ...form, database })}><SelectTrigger id="grant-database"><SelectValue placeholder={t("db.selectDatabase")} /></SelectTrigger><SelectContent>{dbs.map((db) => <SelectItem key={db.name} value={db.name}>{db.name}</SelectItem>)}</SelectContent></Select></div>
        {query.isError && <p role="alert" className="text-xs text-error">{t("db.connectionFailed")}</p>}
        <p className="text-xs text-muted">{t("db.identifierHint")}</p>
        <DialogFooter><Button type="button" variant="ghost" disabled={busy} onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button><Button type="submit" disabled={!valid || busy || query.isError}>{busy ? t("confirm.busy") : t("db.create")}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}

function ResetRootDialog({ open, onOpenChange, version, targetLabel }: DatabaseDialogProps) {
  const t = useT();
  const invalidate = useInvalidate();
  const [pass, setPass] = React.useState("");
  const [mode, setMode] = React.useState("existing");
  const [saved, setSaved] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const close = (value: boolean) => { if (!busy) { if (!value) { setPass(""); setSaved(null); } onOpenChange(value); } };
  const valid = !!pass && !/[\x00-\x1f\x7f]/.test(pass);
  const submit = async () => {
    if (busy || !valid || !version) return;
    setBusy(true);
    try {
      await api.dbResetRootPassword(pass, version, mode === "existing");
      toast.success(t(mode === "existing" ? "db.rootSyncDone" : "db.rootUpdated"));
      setPass(""); setSaved(null); onOpenChange(false); invalidate("databases", "db-users");
    } catch (error) { toastError(error); } finally { setBusy(false); }
  };
  return <Dialog open={open} onOpenChange={close}>
    <DialogContent className="max-w-lg max-h-[85dvh] overflow-y-auto" hideClose={busy}>
      <DialogHeader><DialogTitle>{t("db.rootManage")}</DialogTitle><DialogDescription>{targetLabel}</DialogDescription></DialogHeader>
      <div className="space-y-1.5"><Label htmlFor="root-mode">{t("db.operation")}</Label><Select value={mode} disabled={busy} onValueChange={setMode}><SelectTrigger id="root-mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="existing">{t("db.rootSync")}</SelectItem><SelectItem value="change">{t("db.rootChange")}</SelectItem></SelectContent></Select></div>
      <p className="text-sm text-muted">{t(mode === "existing" ? "db.rootSyncHint" : "db.rootChangeHint")}</p>
      <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <div className="space-y-1.5"><Label htmlFor="root-password">{t("db.rootPasswordLabel")}</Label><Input id="root-password" type="password" disabled={busy} value={pass} onChange={(e) => setPass(e.target.value)} autoComplete={mode === "existing" ? "current-password" : "new-password"} /></div>
        <Button type="button" variant="secondary" disabled={busy} onClick={async () => {
          if (saved !== null) { setSaved(null); return; }
          setBusy(true);
          try { setSaved(await api.dbRootPassword(version)); } catch (error) { toastError(error); } finally { setBusy(false); }
        }}>{t(saved !== null ? "db.hidePassword" : "db.showPassword")}</Button>
        {saved !== null && <div className="flex min-w-0 items-center gap-2 rounded-md bg-fill p-3"><code className="min-w-0 flex-1 break-all text-xs">{saved}</code><CopyButton text={saved} /></div>}
        <DialogFooter><Button type="button" variant="ghost" disabled={busy} onClick={() => close(false)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !valid}>{busy ? t("confirm.busy") : t(mode === "existing" ? "db.rootSync" : "db.rootChange")}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}
