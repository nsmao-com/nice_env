"use client";

import * as React from "react";
import { toast } from "sonner";
import { Database, HardDrive, KeyRound, Play, Plus, Table2, Trash2, UserRound, ExternalLink, Loader2, Import } from "lucide-react";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { DatabaseWorkspace } from "@/components/shared/database-workspace";
import { useDatabases, useDbUsers, useInvalidate, toastError, useAdminer, useServices } from "@/lib/hooks";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { DbBackupCard } from "@/components/shared/db-backup";
import { DbImportDialog } from "@/components/shared/db-import-dialog";
import { ServiceIcon } from "@/components/shared/service-icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
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
import type { DatabaseEngine, DbUserInfo, ServiceStatus } from "@nsb/schema";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { PageHeader } from "@/components/layout/app-shell";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { DatabaseGrantsSheet } from "@/components/shared/database-grants";
import { PostgresManagement } from "@/components/shared/postgres-management";
import { RedisSettingsButton } from "@/components/shared/redis-settings";
import { RedisPersistenceButton } from "@/components/shared/redis-persistence";

export default function DatabasesPage() {
  const t = useT();
  const { data: services } = useServices();
  const [choice, setChoice] = React.useState<string | null>(null);
  const [locked, setLocked] = React.useState(false);
  const selected = choice ?? (services.some((service) => service.id === "postgresql") && !services.some((service) => /^(mysql|mariadb)(@|$)/.test(service.id)) ? "postgresql" : "mysql");
  const postgres = services.find((service) => service.id === "postgresql");
  return <div className="pb-8">
    <PageHeader title={t("db.title")} subtitle={t("db.subtitle")} />
    <Tabs value={selected} onValueChange={setChoice}>
      <TabsList className="mb-5 flex w-fit max-w-full"><TabsTrigger value="mysql" disabled={locked} className="px-2 text-xs sm:px-3 sm:text-sm">MySQL / MariaDB</TabsTrigger><TabsTrigger value="postgresql" disabled={locked} className="px-2 text-xs sm:px-3 sm:text-sm">PostgreSQL</TabsTrigger></TabsList>
      <TabsContent value="mysql"><MySqlWorkspace onLockChange={setLocked} /></TabsContent>
      <TabsContent value="postgresql">
        <div className="mb-5 grid grid-cols-1 gap-3 md:grid-cols-2"><PostgresInstanceCard /><RedisInstanceCard /></div>
        <PostgresManagement service={postgres} onLockChange={setLocked} />
      </TabsContent>
    </Tabs>
  </div>;
}

function UserPasswordDialog({ engine, version, account, targetLabel, signature, changed, onClose }: {
  engine: DatabaseEngine; version: string; account: DbUserInfo; targetLabel: string; signature: string; changed: boolean; onClose: () => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const queryKey = ["db-user-password-info", signature, account.username, account.host];
  const query = useQuery({ queryKey, queryFn: () => api.dbUserPasswordInfo(engine, version, account.username, account.host), enabled: !changed, retry: false, refetchOnWindowFocus: false });
  const info = query.data;
  const [password, setPassword] = React.useState("");
  const [confirmation, setConfirmation] = React.useState("");
  const [showPassword, setShowPassword] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [needsReload, setNeedsReload] = React.useState(false);
  const lock = React.useRef(false);
  const valid = !!password && new TextEncoder().encode(password).length <= 4096 && !/[\x00-\x1f\x7f-\x9f]/.test(password);
  const disabled = busy || changed || query.isFetching || query.isError || needsReload || !info?.supported;
  const close = () => { if (!lock.current) { setPassword(""); setConfirmation(""); onClose(); } };
  const reload = async () => { if (lock.current) return; const result = await query.refetch(); if (!result.isError) { setNeedsReload(false); setError(""); } };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault(); if (lock.current || disabled || !info || !valid || password !== confirmation) return;
    lock.current = true; setBusy(true); setError("");
    try {
      const saved = await api.dbUserPasswordSave(engine, version, { username: account.username, host: account.host, password, revision: info.revision });
      queryClient.setQueryData(queryKey, saved); setPassword(""); setConfirmation("");
      toast.success(t("dbPassword.saved")); onClose();
    } catch (cause) { const parsed = normalizeError(cause); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); setNeedsReload(true); }
    finally { lock.current = false; setBusy(false); }
  };
  return <Dialog open onOpenChange={(open) => !open && close()}><DialogContent hideClose={busy} className="flex max-w-lg max-h-[85dvh] flex-col overflow-hidden">
    <DialogHeader><DialogTitle className="pr-6">{t("dbPassword.title")}</DialogTitle><DialogDescription className="break-words">{targetLabel}<span className="mt-1 block break-all font-mono text-foreground">{account.username || t("dbGrants.anonymous")} @ {account.host || "—"}</span></DialogDescription></DialogHeader>
    <form onSubmit={submit} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
      {changed && <p role="alert" className="text-sm text-error">{t("db.pgChanged")}</p>}
      {query.isPending && <p role="status" className="text-sm text-muted">{t("db.loading")}</p>}
      {query.isError && <p role="alert" className="break-words text-sm text-error">{normalizeError(query.error).message}</p>}
      {info && <>
        <p className="text-xs leading-5 text-muted">{t("dbPassword.scope")}</p>
        <div className="text-xs text-muted"><span>{t("dbPassword.plugins")}: </span><span className="break-all font-mono">{info.plugins.join(" / ") || "—"}</span></div>
        {info.protected ? <p className="rounded-md bg-warn-soft p-3 text-sm text-warn">{t("dbPassword.protected")}</p> : !info.supported ? <p className="text-sm text-warn">{t("dbPassword.unsupported")}</p> : <>
          {info.otherAuthentication && <p className="rounded-md bg-warn-soft p-3 text-xs leading-5 text-warn">{t("dbPassword.otherAuth")} <span className="break-all font-mono">{info.targetPlugin}</span></p>}
          <div className="space-y-4 border-t border-dashed border-border pt-4">
            <div className="space-y-1.5"><Label htmlFor="db-user-password">{t("dbPassword.newPassword")}</Label><Input id="db-user-password" type={showPassword ? "text" : "password"} value={password} disabled={disabled} maxLength={4096} autoComplete="new-password" aria-invalid={!!password && !valid} aria-describedby="db-user-password-help" onChange={(event) => setPassword(event.target.value)} /><p id="db-user-password-help" className={`text-xs ${password && !valid ? "text-error" : "text-muted"}`}>{t("pg.passwordHint")}</p></div>
            <div className="space-y-1.5"><Label htmlFor="db-user-password-confirm">{t("dbPassword.confirmPassword")}</Label><Input id="db-user-password-confirm" type={showPassword ? "text" : "password"} value={confirmation} disabled={disabled} maxLength={4096} autoComplete="new-password" aria-invalid={!!confirmation && confirmation !== password} onChange={(event) => setConfirmation(event.target.value)} />{confirmation && confirmation !== password && <p role="alert" className="text-xs text-error">{t("dbPassword.mismatch")}</p>}</div>
            <label className="flex items-center gap-2 text-xs text-muted"><input type="checkbox" className="h-4 w-4 accent-[var(--primary)]" checked={showPassword} disabled={busy} onChange={(event) => setShowPassword(event.target.checked)} />{t("dbPassword.show")}</label>
          </div>
          <p className="text-xs leading-5 text-muted">{t("dbPassword.connections")}</p>
        </>}
      </>}
      {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
      {(query.isError || needsReload) && !changed && <Button type="button" size="sm" variant="secondary" disabled={busy || query.isFetching} onClick={reload}>{t("dbPassword.reload")}</Button>}
    </div><DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={close}>{t("common.cancel")}</Button>{!info?.protected && info?.supported && <Button type="submit" disabled={disabled || !valid || password !== confirmation}>{busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{t("pg.save")}</Button>}</DialogFooter></form>
  </DialogContent></Dialog>;
}

function UserDropDialog({ engine, version, account, targetLabel, signature, changed, onClose }: {
  engine: DatabaseEngine; version: string; account: DbUserInfo; targetLabel: string; signature: string; changed: boolean; onClose: () => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate();
  const query = useQuery({ queryKey: ["db-user-drop-info", signature, account.username, account.host], queryFn: () => api.dbUserDropInfo(engine, version, account.username, account.host), enabled: !changed, retry: false, refetchOnWindowFocus: false, staleTime: 0 });
  const info = query.data;
  const [confirmation, setConfirmation] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [needsReload, setNeedsReload] = React.useState(false);
  const lock = React.useRef(false);
  const accountLabel = `${account.username}@${account.host}`;
  const queryError = query.isError ? normalizeError(query.error) : null;
  const blocked = !info || info.protected || info.dependencies.length > 0 || info.roleDependents > 0 || info.proxyDependents > 0;
  const disabled = busy || changed || query.isFetching || query.isError || needsReload;
  const close = () => { if (!lock.current) onClose(); };
  const reload = async () => {
    if (lock.current) return;
    const result = await query.refetch();
    if (!result.isError) { setConfirmation(""); setError(""); setNeedsReload(false); }
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault(); if (lock.current || disabled || blocked || !info || confirmation !== accountLabel) return;
    lock.current = true; setBusy(true); setError("");
    try {
      await api.dbUserDrop(engine, version, { username: account.username, host: account.host, confirmation, revision: info.revision });
      queryClient.setQueryData<DbUserInfo[]>(["db-users", engine, version], (users) => users?.filter((user) => user.username !== account.username || user.host !== account.host));
      invalidate("db-users", "db-grants", "db-user-password-info", "db-user-drop-info");
      toast.success(t("dbUserDrop.saved")); onClose();
    } catch (cause) { const parsed = normalizeError(cause); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); setNeedsReload(true); }
    finally { lock.current = false; setBusy(false); }
  };
  return <Dialog open onOpenChange={(open) => !open && close()}><DialogContent hideClose={busy} className="flex max-w-lg max-h-[85dvh] flex-col overflow-hidden">
    <DialogHeader><DialogTitle className="pr-6">{t("dbUserDrop.title")}</DialogTitle><DialogDescription className="break-words">{targetLabel}<span className="mt-1 block break-all font-mono text-foreground">{accountLabel}</span></DialogDescription></DialogHeader>
    <form onSubmit={submit} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
      {changed && <p role="alert" className="text-sm text-error">{t("db.pgChanged")}</p>}
      {query.isPending && <p role="status" className="text-sm text-muted">{t("dbUserDrop.loading")}</p>}
      {queryError && <p role="alert" className="break-words text-sm text-error">{[queryError.message, queryError.hint].filter(Boolean).join(" ")}</p>}
      {info && <>
        <p className="text-sm leading-6 text-muted">{t("dbUserDrop.scope")}</p>
        {info.protected ? <p className="rounded-md bg-warn-soft p-3 text-sm text-warn">{t("dbUserDrop.protected")}</p> : <>
          {blocked && <div className="space-y-2 rounded-md bg-warn-soft p-3 text-xs leading-5 text-warn">
            <p>{t("dbUserDrop.blocked")}</p>
            {info.dependencies.length > 0 && <ul className="space-y-1">{info.dependencies.map((item, index) => <li key={`${item.kind}:${item.database}:${item.name}:${index}`} className="break-words">{t(`dbUserDrop.${item.kind}`)} · <span className="break-all font-mono">{item.database}.{item.name}</span></li>)}</ul>}
            {info.moreDependencies && <p>{t("dbUserDrop.more")}</p>}
            {info.roleDependents > 0 && <p>{t("dbUserDrop.roles")}: {info.roleDependents}</p>}
            {info.proxyDependents > 0 && <p>{t("dbUserDrop.proxies")}: {info.proxyDependents}</p>}
          </div>}
          <div className="space-y-2 text-xs leading-5 text-muted"><p>{t("dbUserDrop.connections")}: {info.usernameConnections}</p><p>{t("dbUserDrop.sessionHint")}</p></div>
          {!blocked && <div className="space-y-2 border-t border-dashed border-border pt-4"><Label htmlFor="db-user-drop-confirm">{t("dbUserDrop.confirm")}</Label><p className="break-all font-mono text-xs">{accountLabel}</p><Input id="db-user-drop-confirm" value={confirmation} autoComplete="off" disabled={disabled} onChange={(event) => setConfirmation(event.target.value)} /></div>}
        </>}
      </>}
      {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
      {(query.isError || needsReload || (!!info && blocked && !info.protected)) && !changed && <Button type="button" size="sm" variant="secondary" className="h-auto min-h-8 max-w-full whitespace-normal py-2" disabled={busy || query.isFetching} onClick={reload}>{t("dbUserDrop.reload")}</Button>}
      {needsReload && <p className="text-xs text-muted">{t("dbUserDrop.reloadHint")}</p>}
    </div><DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={close}>{t("common.cancel")}</Button>{!blocked && <Button type="submit" variant="destructive" disabled={disabled || confirmation !== accountLabel}>{busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{t("dbUserDrop.title")}</Button>}</DialogFooter></form>
  </DialogContent></Dialog>;
}

function MySqlWorkspace({ onLockChange }: { onLockChange: (locked: boolean) => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const serviceQuery = useServices();
  const databaseServices = serviceQuery.data.filter((s) => ["mysql", "mariadb"].includes(s.id.split("@")[0]));
  const [selectedInstance, setSelectedInstance] = React.useState("");
  const instanceKey = (s: ServiceStatus) => `${s.id}:${s.version}`;
  const defaultService = databaseServices.find((s) => s.state === "running") ?? databaseServices[0];
  const selected = selectedInstance || (defaultService ? instanceKey(defaultService) : "");
  React.useEffect(() => { if (!selectedInstance && selected) setSelectedInstance(selected); }, [selectedInstance, selected]);
  const service = databaseServices.find((s) => instanceKey(s) === selected);
  const version = service?.version ?? "";
  const engine: DatabaseEngine = service?.id.split("@")[0] === "mariadb" ? "mariadb" : "mysql";
  const engineLabel = engine === "mariadb" ? "MariaDB" : "MySQL";
  const running = !!version && (service?.state === "running" || (service?.state === "error" && service.pids.length > 0));
  const dbQuery = useDatabases(version, running, engine);
  const userQuery = useDbUsers(version, running, engine);
  const dbs = dbQuery.data ?? [];
  const users = userQuery.data ?? [];
  const ready = running && dbQuery.isSuccess;
  const targetLabel = `${engineLabel} ${version} · 127.0.0.1:${service?.port ?? "—"}`;
  const [workspaceLocked, setWorkspaceLocked] = React.useState(false);
  const [backupLocked, setBackupLocked] = React.useState(false);
  const [createOpen, setCreateOpen] = React.useState(false);
  const [userOpen, setUserOpen] = React.useState(false);
  const [grantUser, setGrantUser] = React.useState<DbUserInfo | null>(null);
  const [passwordUser, setPasswordUser] = React.useState<{ account: DbUserInfo; signature: string } | null>(null);
  const [deleteUser, setDeleteUser] = React.useState<{ account: DbUserInfo; signature: string } | null>(null);
  const signature = `${engine}:${version}:${service?.port}:${service?.pids.join(",")}`;
  const [rootOpen, setRootOpen] = React.useState(false);
  const [importOpen, setImportOpen] = React.useState(false);
  const [dropTarget, setDropTarget] = React.useState<string | null>(null);
  const [dropping, setDropping] = React.useState(false);
  const [dropTyped, setDropTyped] = React.useState("");
  const locked = workspaceLocked || !!deleteUser || !!passwordUser || !!grantUser || backupLocked || createOpen || userOpen || rootOpen || importOpen || !!dropTarget;
  React.useEffect(() => { onLockChange(locked); return () => onLockChange(false); }, [locked, onLockChange]);

  const systemDbs = new Set(["mysql", "sys", "information_schema", "performance_schema"]);

  return (
    <div className="pb-8">
      <DbImportDialog key={`import-${engine}-${version}`} open={importOpen} onOpenChange={setImportOpen} version={version} engine={engine} targetLabel={targetLabel} />
      <div className="mb-5 flex flex-wrap justify-end gap-2">
            <Button variant="secondary" disabled={!running} onClick={() => setRootOpen(true)}>
              <KeyRound className="h-3.5 w-3.5" /> {t("db.rootManage")}
            </Button>
            <Button variant="secondary" disabled={!ready} onClick={() => setImportOpen(true)}>
              <Import className="h-3.5 w-3.5" /> {t("dbImport.open")}
            </Button>
            <Button disabled={!ready} onClick={() => setCreateOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("db.createDb")}
            </Button>
      </div>

      <div className="mb-5 flex flex-wrap items-end gap-3">
        <div className="w-full max-w-sm space-y-1.5">
          <Label htmlFor="database-instance">{t("db.chooseInstance")}</Label>
          <Select value={selected} onValueChange={setSelectedInstance} disabled={locked || !databaseServices.length}>
            <SelectTrigger id="database-instance"><SelectValue placeholder={t("db.chooseInstance")} /></SelectTrigger>
            <SelectContent>{databaseServices.map((s) => <SelectItem key={s.id} value={instanceKey(s)}>{s.id.split("@")[0] === "mariadb" ? "MariaDB" : "MySQL"} {s.version} · {s.port ?? "—"} · {s.state === "running" ? t("common.running") : s.state === "error" ? t("common.error") : t("common.stopped")}</SelectItem>)}</SelectContent>
          </Select>
        </div>
        <Button variant="secondary" disabled={!running || dbQuery.isFetching || userQuery.isFetching} onClick={() => invalidate("databases", "db-users")}>{t("db.refresh")}</Button>
      </div>
      {serviceQuery.isError ? <p role="alert" className="mb-4 text-sm text-error">{t("db.connectionFailed")} <Button variant="ghost" onClick={() => void serviceQuery.refetch()}>{t("db.retry")}</Button></p> : !databaseServices.length ? <p className="mb-4 text-sm text-muted">{serviceQuery.isFetching ? t("db.loading") : t("db.noInstance")}</p> : null}
      {/* 所选 MySQL / MariaDB 实例与 Redis */}
      <section className="mb-6">
        <SectionHeader title={t("db.instance")} className="mb-3" />
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          <MySqlInstanceCard key={selected} engine={engine} service={service} count={ready ? dbs.length : undefined} />
          <RedisInstanceCard />
        </div>
      </section>

      <DatabaseWorkspace key={signature} engine={engine} version={version} databases={dbs.map((db) => db.name)} ready={ready} targetLabel={targetLabel} onLockChange={setWorkspaceLocked} />
      {/* 备份 / 还原 */}
      <section className="mb-6">
        <DbBackupCard key={selected} version={version} engine={engine} targetLabel={targetLabel} ready={ready} databases={dbs.filter((db) => !systemDbs.has(db.name.toLowerCase())).map((db) => db.name)} onLockChange={setBackupLocked} />
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
                <div key={JSON.stringify([u.username, u.host])} className="min-w-0 rounded-md bg-fill px-3 py-2">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="break-all font-mono text-[12px]">{u.username || t("dbGrants.anonymous")}</span>
                    <span className="break-all text-[10px] text-faint">@{u.host}</span>
                  </div>
                  {u.grants && <p className="mt-0.5 break-words text-[10.5px] text-faint">{u.grants}</p>}
                  <div className="mt-1 flex flex-wrap items-center gap-1"><Button size="sm" variant="ghost" disabled={!ready || locked} onClick={() => setGrantUser(u)}>{t("dbGrants.manage")}</Button><Button size="sm" variant="ghost" disabled={!ready || locked} onClick={() => setPasswordUser({ account: u, signature })}><KeyRound className="h-3.5 w-3.5" />{t("dbPassword.title")}</Button><Button size="icon-sm" variant="ghost" className="ml-auto text-error" title={t("dbUserDrop.title")} aria-label={`${t("dbUserDrop.title")} ${u.username}@${u.host}`} disabled={!ready || locked} onClick={() => setDeleteUser({ account: u, signature })}><Trash2 className="h-3.5 w-3.5" /></Button></div>
                </div>
              ))
            )}
          </CardContent>
        </Card>
      </div>

      <CreateDbDialog key={`create-db-${engine}-${version}`} version={version} engine={engine} targetLabel={targetLabel} open={createOpen} onOpenChange={setCreateOpen} onDone={() => invalidate("databases")} />
      <CreateUserDialog key={`create-user-${engine}-${version}`} version={version} engine={engine} targetLabel={targetLabel} open={userOpen} onOpenChange={setUserOpen} onDone={() => invalidate("db-users")} />
      {grantUser && <DatabaseGrantsSheet key={JSON.stringify([engine, version, grantUser.username, grantUser.host])} engine={engine} version={version} account={grantUser} targetLabel={targetLabel} ready={ready} onClose={() => setGrantUser(null)} />}
      {passwordUser && <UserPasswordDialog engine={engine} version={version} account={passwordUser.account} targetLabel={targetLabel} signature={passwordUser.signature} changed={!running || signature !== passwordUser.signature} onClose={() => setPasswordUser(null)} />}
      {deleteUser && <UserDropDialog engine={engine} version={version} account={deleteUser.account} targetLabel={targetLabel} signature={deleteUser.signature} changed={!running || signature !== deleteUser.signature} onClose={() => setDeleteUser(null)} />}
      <ResetRootDialog key={`root-${engine}-${version}`} version={version} engine={engine} targetLabel={targetLabel} open={rootOpen} onOpenChange={setRootOpen} />

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
            await api.dbDrop(dropTarget, version, engine);
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
  if (svc.state === "running" || (svc.state === "error" && svc.pids.length > 0)) {
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

function MySqlInstanceCard({ service, count, engine }: { service?: ServiceStatus; count?: number; engine: DatabaseEngine }) {
  const t = useT();
  const adminer = useAdminer();
  const phpMyAdmin = useAdminer("phpmyadmin");
  const port = service?.port;
  return (
    <Card className="min-w-0 p-4">
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <div className="flex h-9 w-9 items-center justify-center rounded-md bg-fill">
          <ServiceIcon id={engine} className="h-[18px] w-[18px]" />
        </div>
        <div>
          <p className="text-[13px] font-medium">{engine === "mariadb" ? "MariaDB" : "MySQL"} {service?.version}</p>
          <p className="text-[11px] text-faint">127.0.0.1:{port ?? "—"} · utf8mb4</p>
        </div>
        <div className="ml-auto">
          {service && <InstanceStartButton base={engine} version={service.version} />}
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
          ["CLI", `${engine === "mariadb" ? "mariadb" : "mysql"} -u root -p -h 127.0.0.1 -P ${port ?? "—"}`],
          ["PDO", `"mysql:host=127.0.0.1;port=${port ?? "—"};dbname=dbname"`],
        ].map(([k, v]) => (
          <div key={k} className="flex items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5">
            <span className="w-7 shrink-0 text-[10px] font-medium text-faint">{k}</span>
            <code className="min-w-0 flex-1 truncate font-mono text-[11px] text-secondary">{v}</code>
            <CopyButton text={v} />
          </div>
        ))}
      </div>
      <div className="mt-3 flex flex-wrap gap-2">
        <Button variant="secondary" size="sm" disabled={adminer.busy} onClick={() => void adminer.open()}>
          {adminer.busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <ExternalLink className="h-3.5 w-3.5" />} {t("db.openAdminer")}
        </Button>
        <Button variant="secondary" size="sm" disabled={phpMyAdmin.busy || adminer.busy} onClick={() => void phpMyAdmin.open()}><ExternalLink className="h-3.5 w-3.5" />phpMyAdmin</Button>
        {adminer.query.data && <Button size="sm" variant="ghost" disabled={adminer.busy || phpMyAdmin.busy} onClick={() => void adminer.stop()}>停止 {adminer.query.data.packageId === "phpmyadmin" ? "phpMyAdmin" : "Adminer"}</Button>}
      </div>
    </Card>
  );
}

function PostgresInstanceCard() {
  const t = useT();
  const service = useInstanceState("postgresql");
  const running = !!service?.version && (service.state === "running" || (service.state === "error" && service.pids.length > 0));
  const [target, setTarget] = React.useState<ServiceStatus | null>(null);
  const query = useQuery({ queryKey: ["postgres-connection", service?.version, service?.port, service?.pids.join(",")],
    queryFn: () => api.postgresConnection(service!.version!), enabled: running, retry: false, staleTime: 15000, refetchOnWindowFocus: false });
  const info = running && !query.isError ? query.data : undefined;
  const error = query.error ? normalizeError(query.error) : null;
  return <>
    <Card className="min-w-0 p-4">
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-fill"><ServiceIcon id="postgresql" className="h-[18px] w-[18px]" /></div>
        <div className="min-w-0 flex-1"><p className="text-[13px] font-medium">PostgreSQL {service?.version}</p><p className="text-[11px] text-faint">127.0.0.1:{service?.port ?? "—"} · postgres</p></div>
        {service && <InstanceStartButton base="postgresql" />}
      </div>
      {!running ? <p className="text-xs text-muted">{t(service ? "db.pgStopped" : "db.pgNotInstalled")}</p> : <>
        {query.isPending && <p role="status" className="mb-3 text-xs text-muted">{t("db.loading")}</p>}
        {error && <div role="alert" className="mb-3 space-y-1 rounded-md bg-warning-soft p-2.5 text-xs text-muted"><p className="break-words">{error.message}</p>{error.hint && <p className="break-words">{error.hint}</p>}</div>}
        {info && <>
          <div className="mb-2 flex flex-wrap gap-1.5"><StatChip icon={Database}>{info.databaseCount} {t("db.dbs")}</StatChip><StatChip icon={HardDrive}>{fmtBytes(info.sizeBytes)}</StatChip></div>
          <p className={`mb-3 text-xs ${info.passwordRequired ? "text-success" : "text-warning"}`}>{t(info.passwordRequired ? "db.pgAuthenticated" : "db.pgTrust")}</p>
        </>}
        <div className="mb-3 flex flex-wrap gap-2"><Button size="sm" variant="secondary" onClick={() => service && setTarget(service)}><KeyRound className="h-3.5 w-3.5" />{t("db.pgPassword")}</Button><Button size="sm" variant="ghost" disabled={query.isFetching || !!target} onClick={() => void query.refetch()}>{t(query.isError ? "db.retry" : "db.refresh")}</Button></div>
        {info && <div className="space-y-1.5"><p className="text-[11px] font-medium text-secondary">{t("db.connStrings")}</p>
          {[["URL", `postgresql://postgres@127.0.0.1:${info.port}/postgres`], ["CLI", `psql -h 127.0.0.1 -p ${info.port} -U postgres -d postgres -W`]].map(([label, value]) => <div key={label} className="flex min-w-0 items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5"><span className="w-7 shrink-0 text-[10px] font-medium text-faint">{label}</span><code className="min-w-0 flex-1 truncate font-mono text-[11px] text-secondary" title={value}>{value}</code><CopyButton text={value} /></div>)}
        </div>}
      </>}
    </Card>
    {target?.version && <PostgresPasswordDialog key={`${target.version}-${target.pids.join(",")}`} service={target} passwordRequired={info?.passwordRequired} onClose={() => setTarget(null)} />}
  </>;
}

function PostgresPasswordDialog({ service, passwordRequired, onClose }: { service: ServiceStatus; passwordRequired?: boolean; onClose: () => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const version = service.version!;
  const current = useInstanceState("postgresql");
  const changed = !current || current.version !== version || current.port !== service.port || current.pids.join(",") !== service.pids.join(",") || !["running", "error"].includes(current.state);
  const [mode, setMode] = React.useState(passwordRequired === false ? "change" : "existing");
  const [password, setPassword] = React.useState("");
  const [enableAuth, setEnableAuth] = React.useState(true);
  const [saved, setSaved] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const lock = React.useRef(false);
  const [error, setError] = React.useState("");
  const valid = !!password && !/[\x00-\x1f\x7f-\x9f]/.test(password);
  React.useEffect(() => { if (changed) setSaved(null); }, [changed]);
  const report = (error: unknown) => { const detail = normalizeError(error); setError([detail.message, detail.hint].filter(Boolean).join(" ")); };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (lock.current || changed || !valid) return;
    lock.current = true; setBusy(true); setError(""); setSaved(null);
    try {
      await api.postgresSetPassword(version, password, mode === "existing", enableAuth);
      setPassword(""); toast.success(t(mode === "existing" ? "db.rootSyncDone" : "db.pgUpdated")); onClose();
    } catch (error) { report(error); }
    finally { lock.current = false; setBusy(false); invalidate("postgres-connection", "services"); }
  };
  return <Dialog open onOpenChange={(value) => !value && !lock.current && onClose()}>
    <DialogContent hideClose={busy} className="flex max-w-lg max-h-[85dvh] flex-col overflow-hidden">
      <DialogHeader><DialogTitle>{t("db.pgPassword")}</DialogTitle><DialogDescription>PostgreSQL {version} · 127.0.0.1:{service.port}</DialogDescription></DialogHeader>
      <form onSubmit={submit} className="flex min-h-0 flex-col gap-4">
        <div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
          {changed && <p role="alert" className="text-sm text-error">{t("db.pgChanged")}</p>}
          <div className="space-y-1.5"><Label htmlFor="pg-password-mode">{t("db.operation")}</Label><Select value={mode} disabled={busy || changed} onValueChange={(value) => { setMode(value); setPassword(""); setSaved(null); setError(""); }}><SelectTrigger id="pg-password-mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="existing">{t("db.rootSync")}</SelectItem><SelectItem value="change">{t("db.rootChange")}</SelectItem></SelectContent></Select></div>
          <p className="text-xs text-muted">{t(mode === "existing" ? "db.pgSyncHint" : "db.rootChangeHint")}</p>
          <div className="space-y-1.5"><Label htmlFor="pg-password">{t("db.pgPassword")}</Label><Input id="pg-password" type="password" disabled={busy || changed} value={password} autoComplete={mode === "existing" ? "current-password" : "new-password"} onChange={(event) => setPassword(event.target.value)} />{password && !valid && <p role="alert" className="text-xs text-error">{t("db.pgPasswordInvalid")}</p>}</div>
          {mode === "change" && <div className="space-y-2 rounded-md border border-border p-3"><div className="flex items-center justify-between gap-3"><Label htmlFor="pg-enable-auth" className="leading-5">{t("db.pgEnableAuth")}</Label><Switch id="pg-enable-auth" checked={enableAuth} disabled={busy || changed} onCheckedChange={setEnableAuth} /></div><p className="text-xs text-muted">{t(enableAuth ? "db.pgEnableAuthHint" : "db.pgAuthUnchanged")}</p></div>}
          <Button type="button" variant="secondary" disabled={busy || changed} onClick={async () => {
            if (lock.current) return;
            if (saved !== null) { setSaved(null); return; }
            lock.current = true; setBusy(true); setError("");
            try { setSaved(await api.postgresPassword(version)); } catch (error) { report(error); }
            finally { lock.current = false; setBusy(false); }
          }}>{t(saved === null ? "db.showPassword" : "db.hidePassword")}</Button>
          {!changed && saved !== null && <div className="flex min-w-0 items-center gap-2 rounded-md bg-fill p-3"><code className="min-w-0 flex-1 break-all text-xs">{saved}</code><CopyButton text={saved} /></div>}
          {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
        </div>
        <DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={onClose}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || changed || !valid}>{busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}{t(mode === "existing" ? "db.redisVerifySave" : "db.rootChange")}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
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
      <div className="mb-3 flex flex-wrap gap-2"><RedisSettingsButton version={service?.version} /><RedisPersistenceButton version={service?.version} running={running} /></div>
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
  engine: DatabaseEngine;
  targetLabel: string;
  onDone?: () => void;
};

function CreateDbDialog({ open, onOpenChange, onDone, version, engine, targetLabel }: DatabaseDialogProps) {
  const t = useT();
  const [name, setName] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const valid = /^[A-Za-z0-9_]{1,64}$/.test(name);
  const submit = async () => {
    if (busy || !valid || !version) return;
    setBusy(true);
    try {
      await api.dbCreate(name, version, engine);
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

function CreateUserDialog({ open, onOpenChange, onDone, version, engine, targetLabel }: DatabaseDialogProps) {
  const t = useT();
  const [form, setForm] = React.useState({ username: "", password: "", database: "" });
  const [busy, setBusy] = React.useState(false);
  const query = useDatabases(version, open && !!version, engine);
  const dbs = (query.data ?? []).filter((db) => !["mysql", "sys", "information_schema", "performance_schema"].includes(db.name.toLowerCase()));
  const valid = /^[A-Za-z0-9_]{1,32}$/.test(form.username) && form.username.toLowerCase() !== "root" && !!form.password && !/[\x00-\x1f\x7f]/.test(form.password) && dbs.some((db) => db.name === form.database);
  const submit = async () => {
    if (busy || !valid || query.isError) return;
    setBusy(true);
    try {
      await api.dbCreateUser(form.username, form.password, form.database, version, engine);
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

function ResetRootDialog({ open, onOpenChange, version, engine, targetLabel }: DatabaseDialogProps) {
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
      await api.dbResetRootPassword(pass, version, mode === "existing", engine);
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
          try { setSaved(await api.dbRootPassword(version, engine)); } catch (error) { toastError(error); } finally { setBusy(false); }
        }}>{t(saved !== null ? "db.hidePassword" : "db.showPassword")}</Button>
        {saved !== null && <div className="flex min-w-0 items-center gap-2 rounded-md bg-fill p-3"><code className="min-w-0 flex-1 break-all text-xs">{saved}</code><CopyButton text={saved} /></div>}
        <DialogFooter><Button type="button" variant="ghost" disabled={busy} onClick={() => close(false)}>{t("common.cancel")}</Button><Button type="submit" disabled={busy || !valid}>{busy ? t("confirm.busy") : t(mode === "existing" ? "db.rootSync" : "db.rootChange")}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}
