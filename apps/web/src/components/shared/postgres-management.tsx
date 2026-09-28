"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Database, KeyRound, Loader2, Plus, Settings2, Trash2, UserRound } from "lucide-react";
import { toast } from "sonner";
import type { ServiceStatus } from "@nsb/schema";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useInvalidate } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { CopyButton } from "@/components/shared/misc";
import { PostgresBackupCard } from "@/components/shared/postgres-backup";

type Action = { kind: "create-database" | "create-role" | "password" | "drop-database" | "drop-role"; signature: string; name?: string; oid?: number };
const validName = (name: string) => /^[A-Za-z0-9_]{1,63}$/.test(name) && !/^pg_/i.test(name) && !/^(postgres|template0|template1)$/i.test(name);
const validPassword = (password: string) => !!password && new TextEncoder().encode(password).length <= 4096 && !/[\x00-\x1f\x7f-\x9f]/.test(password);
const PAGE_SIZE = 10;

export function PostgresManagement({ service, onLockChange }: { service?: ServiceStatus; onLockChange: (locked: boolean) => void }) {
  const t = useT();
  const invalidate = useInvalidate();
  const version = service?.version ?? "";
  const running = !!version && (service?.state === "running" || (service?.state === "error" && service.pids.length > 0));
  const signature = `${version}:${service?.port}:${service?.pids.join(",")}`;
  const databases = useQuery({ queryKey: ["postgres-databases", signature], queryFn: () => api.postgresDatabases(version), enabled: running, retry: false });
  const roleQuery = useQuery({ queryKey: ["postgres-roles", signature], queryFn: () => api.postgresRoles(version), enabled: running, retry: false });
  const dbs = running && !databases.isError ? databases.data ?? [] : [];
  const roles = running && !roleQuery.isError ? roleQuery.data ?? [] : [];
  const [backupLocked, setBackupLocked] = React.useState(false);
  const [accessRole, setAccessRole] = React.useState<{ name: string; oid: number; signature: string } | null>(null);
  const ready = running && databases.isSuccess && roleQuery.isSuccess && !backupLocked && !accessRole;
  const [dbSearch, setDbSearch] = React.useState("");
  const [roleSearch, setRoleSearch] = React.useState("");
  const [dbPage, setDbPage] = React.useState(1);
  const [rolePage, setRolePage] = React.useState(1);
  const [action, setAction] = React.useState<Action | null>(null);
  const [name, setName] = React.useState("");
  const [owner, setOwner] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [confirmation, setConfirmation] = React.useState("");
  const [error, setError] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const lock = React.useRef(false);
  const changed = !!action && (!running || action.signature !== signature);
  const deleting = action?.kind === "drop-database" || action?.kind === "drop-role";
  const filteredDbs = dbs.filter((db) => `${db.name} ${db.owner}`.toLowerCase().includes(dbSearch.toLowerCase()));
  const filteredRoles = roles.filter((role) => role.name.toLowerCase().includes(roleSearch.toLowerCase()));
  const dbPages = Math.max(1, Math.ceil(filteredDbs.length / PAGE_SIZE));
  const rolePages = Math.max(1, Math.ceil(filteredRoles.length / PAGE_SIZE));
  const currentDbPage = Math.min(dbPage, dbPages);
  const currentRolePage = Math.min(rolePage, rolePages);
  React.useEffect(() => { onLockChange(!!action || !!accessRole || backupLocked); return () => onLockChange(false); }, [action, accessRole, backupLocked, onLockChange]);
  React.useEffect(() => { setDbPage(1); setRolePage(1); }, [signature]);
  const refresh = () => invalidate("postgres-databases", "postgres-roles", "postgres-connection");
  const begin = (kind: Action["kind"], target?: { name: string; oid: number }) => {
    setAction({ kind, signature, ...target }); setName(""); setPassword(""); setConfirmation(""); setError("");
    setOwner(roles.find((role) => role.name === "postgres" && role.canLogin)?.name ?? roles.find((role) => role.canLogin)?.name ?? "");
  };
  const close = () => { if (!lock.current) { setAction(null); setPassword(""); setError(""); } };
  const title = action?.kind === "create-database" ? t("db.createDb") : action?.kind === "create-role" ? t("db.createUser") : action?.kind === "password" ? t("pg.changePassword") : action?.kind === "drop-database" ? t("pg.deleteDatabase") : t("pg.deleteRole");
  const valid = action && !changed && (deleting ? confirmation === action.name : action.kind === "create-database" ? validName(name) && roles.some((role) => role.name === owner && role.canLogin) : validPassword(password) && (action.kind === "password" || validName(name)));
  const submit = async (event: React.FormEvent) => {
    event.preventDefault(); if (lock.current || !action || !valid) return;
    lock.current = true; setBusy(true); setError("");
    try {
      switch (action.kind) {
        case "create-database": await api.postgresCreateDatabase(version, name, owner); break;
        case "create-role": await api.postgresCreateRole(version, name, password); break;
        case "password": await api.postgresSetRolePassword(version, action.name!, action.oid!, password); break;
        case "drop-database": await api.postgresDropDatabase(version, action.name!, action.oid!); break;
        case "drop-role": await api.postgresDropRole(version, action.name!, action.oid!); break;
      }
      toast.success(t(action.kind === "create-role" ? "pg.roleCreated" : "pg.saved")); setAction(null); setPassword("");
    } catch (error) { const parsed = normalizeError(error); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); }
    finally { lock.current = false; setBusy(false); refresh(); }
  };
  const stateMessage = (query: typeof databases | typeof roleQuery) => !running ? t("db.pgStopped") : query.isPending ? t("db.loading") : query.isError ? normalizeError(query.error).message : null;

  return <div className="space-y-4">
    <PostgresBackupCard version={version} port={service?.port} signature={signature} ready={running && databases.isSuccess && roleQuery.isSuccess && !action && !accessRole} databases={dbs} roles={roles} onLockChange={setBackupLocked} />
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div className="min-w-0"><h2 className="text-sm font-semibold">{t("pg.manage")}</h2><p className="mt-1 break-words text-xs text-muted">PostgreSQL {version || "—"} · 127.0.0.1:{service?.port ?? "—"}</p></div>
      <div className="flex flex-wrap gap-2"><Button variant="secondary" disabled={!running || databases.isFetching || roleQuery.isFetching} onClick={refresh}>{t("db.refresh")}</Button><Button variant="secondary" disabled={!ready} onClick={() => begin("create-role")}><UserRound className="h-3.5 w-3.5" />{t("db.createUser")}</Button><Button disabled={!ready} onClick={() => begin("create-database")}><Plus className="h-3.5 w-3.5" />{t("db.createDb")}</Button></div>
    </div>
    <p className="text-xs leading-5 text-muted">{t("pg.ownerHint")}</p>
    <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
      <Card className="min-w-0"><CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Database className="h-4 w-4" />{t("db.databases")} <span className="text-muted">{running && databases.isSuccess ? dbs.length : "—"}</span></CardTitle></CardHeader><CardContent>
        <Input aria-label={t("pg.searchDatabases")} placeholder={t("pg.searchDatabases")} value={dbSearch} onChange={(event) => { setDbSearch(event.target.value); setDbPage(1); }} className="mb-3" />
        {stateMessage(databases) ? <p role={databases.isError ? "alert" : "status"} className="py-5 text-xs text-muted">{stateMessage(databases)}</p> : !filteredDbs.length ? <p className="py-5 text-xs text-muted">{t("pg.noMatches")}</p> : <div className="divide-y divide-dashed divide-border">{filteredDbs.slice((currentDbPage - 1) * PAGE_SIZE, currentDbPage * PAGE_SIZE).map((db) => <div key={db.oid} className="flex flex-wrap items-center gap-2 py-3">
          <div className="min-w-0 flex-1 basis-36"><p className="truncate font-mono text-sm" title={db.name}>{db.name}</p><p className="mt-1 break-all text-xs text-muted">{t("pg.owner")}: {db.owner} · {db.encoding} · {fmtBytes(db.sizeBytes)}</p></div>
          {db.protected && <Badge variant="muted">{t("db.systemDb")}</Badge>}{!db.allowConnections && <Badge variant="muted">{t("pg.noLogin")}</Badge>}
          {db.allowConnections && <CopyButton text={`postgresql://${encodeURIComponent(db.owner)}@127.0.0.1:${service?.port}/${encodeURIComponent(db.name)}`} />}
          {!db.protected && <Button size="icon-sm" variant="ghost" disabled={!ready} className="text-faint hover:text-error" aria-label={`${t("pg.deleteDatabase")} ${db.name}`} onClick={() => begin("drop-database", db)}><Trash2 className="h-3.5 w-3.5" /></Button>}
        </div>)}</div>}
        {filteredDbs.length > PAGE_SIZE && <Pager page={currentDbPage} pages={dbPages} onChange={setDbPage} />}
      </CardContent></Card>
      <Card className="min-w-0"><CardHeader><CardTitle className="flex items-center gap-2 text-sm"><UserRound className="h-4 w-4" />{t("db.users")} <span className="text-muted">{running && roleQuery.isSuccess ? roles.length : "—"}</span></CardTitle></CardHeader><CardContent>
        <Input aria-label={t("pg.searchRoles")} placeholder={t("pg.searchRoles")} value={roleSearch} onChange={(event) => { setRoleSearch(event.target.value); setRolePage(1); }} className="mb-3" />
        {stateMessage(roleQuery) ? <p role={roleQuery.isError ? "alert" : "status"} className="py-5 text-xs text-muted">{stateMessage(roleQuery)}</p> : !filteredRoles.length ? <p className="py-5 text-xs text-muted">{t("pg.noMatches")}</p> : <div className="divide-y divide-dashed divide-border">{filteredRoles.slice((currentRolePage - 1) * PAGE_SIZE, currentRolePage * PAGE_SIZE).map((role) => <div key={role.oid} className="space-y-2 py-3">
          <div className="flex flex-wrap items-center gap-2"><span className="min-w-0 flex-1 basis-28 break-all font-mono text-sm">{role.name}</span><Badge variant="muted">{t(role.superuser ? "pg.superuser" : role.canLogin ? "pg.loginRole" : "pg.noLogin")}</Badge>
            <Button size="icon-sm" variant="ghost" disabled={!ready} aria-label={`${t("pgAccess.title")} ${role.name}`} onClick={() => setAccessRole({ name: role.name, oid: role.oid, signature })}><Settings2 className="h-3.5 w-3.5" /></Button>
            {!role.protected && <>{role.canLogin && <Button size="icon-sm" variant="ghost" disabled={!ready} aria-label={`${t("pg.changePassword")} ${role.name}`} onClick={() => begin("password", role)}><KeyRound className="h-3.5 w-3.5" /></Button>}<Button size="icon-sm" variant="ghost" disabled={!ready} className="text-faint hover:text-error" aria-label={`${t("pg.deleteRole")} ${role.name}`} onClick={() => begin("drop-role", role)}><Trash2 className="h-3.5 w-3.5" /></Button></>}
          </div>
          <p className="break-words text-xs text-muted">{t("pg.ownedDatabases")}: {role.databases.join(", ") || "—"}</p>
          {!role.superuser && <p className="break-words text-xs text-muted">{t("pgAccess.limit")}: {role.connectionLimit === -1 ? t("pgAccess.unlimited") : role.connectionLimit}{role.connectionLimit === 0 && ` · ${t("pgAccess.zeroShort")}`}</p>}
          {!role.superuser && (role.createDb || role.createRole || role.replication || role.bypassRls) && <p className="text-xs text-warn">{t("pg.extraPrivileges")}: {[role.createDb && "CREATEDB", role.createRole && "CREATEROLE", role.replication && "REPLICATION", role.bypassRls && "BYPASSRLS"].filter(Boolean).join(", ")}</p>}
        </div>)}</div>}
        {filteredRoles.length > PAGE_SIZE && <Pager page={currentRolePage} pages={rolePages} onChange={setRolePage} />}
      </CardContent></Card>
    </div>
    {accessRole && <RoleAccessDialog version={version} port={service?.port} target={accessRole} changed={!running || accessRole.signature !== signature} onClose={() => setAccessRole(null)} onSaved={refresh} />}
    <Dialog open={!!action} onOpenChange={(open) => !open && close()}><DialogContent hideClose={busy} className="flex max-w-lg max-h-[85dvh] flex-col overflow-hidden">
      <DialogHeader><DialogTitle className="pr-6 break-words">{title}{action?.name ? ` · ${action.name}` : ""}</DialogTitle><DialogDescription>PostgreSQL {version} · 127.0.0.1:{service?.port}</DialogDescription></DialogHeader>
      <form onSubmit={submit} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
        {changed && <p role="alert" className="text-sm text-error">{t("db.pgChanged")}</p>}
        {deleting ? <>
          <p className="text-sm text-muted">{t(action?.kind === "drop-database" ? "pg.dropDatabaseHint" : "pg.dropRoleHint")}</p>
          <div className="space-y-1.5"><Label htmlFor="pg-confirm">{t("pg.confirmName")}</Label><Input id="pg-confirm" value={confirmation} disabled={busy || changed} placeholder={action?.name} autoComplete="off" onChange={(event) => setConfirmation(event.target.value)} /></div>
        </> : <>
          {action?.kind !== "password" && <div className="space-y-1.5"><Label htmlFor="pg-object-name">{t(action?.kind === "create-database" ? "pg.databaseName" : "db.username")}</Label><Input id="pg-object-name" maxLength={63} value={name} disabled={busy || changed} onChange={(event) => setName(event.target.value)} autoComplete="off" /><p className={`text-xs ${name && !validName(name) ? "text-error" : "text-muted"}`}>{t("pg.nameHint")}</p></div>}
          {action?.kind === "create-database" ? <div className="space-y-1.5"><Label htmlFor="pg-db-owner">{t("pg.owner")}</Label><Select value={owner} disabled={busy || changed || roleQuery.isError} onValueChange={setOwner}><SelectTrigger id="pg-db-owner"><SelectValue placeholder={t("pg.chooseOwner")} /></SelectTrigger><SelectContent>{roles.filter((role) => role.canLogin).map((role) => <SelectItem key={role.oid} value={role.name}>{role.name}</SelectItem>)}</SelectContent></Select><p className="text-xs text-muted">{t("pg.ownerHint")}</p></div> : <>
            <p className="text-xs text-muted">{t(action?.kind === "password" ? "pg.changePasswordHint" : "pg.roleHint")}</p>
            <div className="space-y-1.5"><Label htmlFor="pg-role-password">{t("db.password")}</Label><Input id="pg-role-password" type="password" value={password} disabled={busy || changed} maxLength={4096} autoComplete="new-password" onChange={(event) => setPassword(event.target.value)} />{password && !validPassword(password) && <p className="text-xs text-error">{t("pg.passwordHint")}</p>}</div>
          </>}
        </>}
        {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
      </div><DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={close}>{t("common.cancel")}</Button><Button type="submit" variant={deleting ? "destructive" : "default"} disabled={busy || !valid}>{busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{deleting ? t("pg.delete") : t("pg.save")}</Button></DialogFooter></form>
    </DialogContent></Dialog>
  </div>;
}

function RoleAccessDialog({ version, port, target, changed, onClose, onSaved }: {
  version: string; port?: number; target: { name: string; oid: number; signature: string }; changed: boolean; onClose: () => void; onSaved: () => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const queryKey = ["postgres-role-access", target.signature, target.oid];
  const query = useQuery({ queryKey, queryFn: () => api.postgresRoleAccess(version, target.name, target.oid), retry: false, refetchOnWindowFocus: false, enabled: !changed });
  const [snapshot, setSnapshot] = React.useState<api.PostgresRoleAccess | null>(null);
  const [canLogin, setCanLogin] = React.useState(true);
  const [unlimited, setUnlimited] = React.useState(true);
  const [limit, setLimit] = React.useState("20");
  const [confirmed, setConfirmed] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [needsReload, setNeedsReload] = React.useState(false);
  const lock = React.useRef(false);
  const apply = React.useCallback((access: api.PostgresRoleAccess) => {
    setSnapshot(access); setCanLogin(access.canLogin); setUnlimited(access.connectionLimit === -1); setLimit(String(access.connectionLimit === -1 ? 20 : access.connectionLimit)); setConfirmed(false); setError(""); setNeedsReload(false);
  }, []);
  React.useEffect(() => { if (query.data) apply(query.data); }, [query.data, apply]);
  const nextLimit = unlimited ? -1 : Number(limit);
  const validLimit = unlimited || (/^\d+$/.test(limit) && Number.isSafeInteger(nextLimit) && nextLimit <= 2147483647);
  const restricting = !!snapshot && ((snapshot.canLogin && !canLogin) || (nextLimit >= 0 && (snapshot.connectionLimit === -1 || nextLimit < snapshot.connectionLimit)));
  const dirty = !!snapshot && (snapshot.canLogin !== canLogin || snapshot.connectionLimit !== nextLimit);
  const disabled = busy || changed || query.isFetching || query.isError || needsReload || !snapshot || snapshot.protected;
  const close = () => { if (!lock.current) onClose(); };
  const reload = async () => { if (lock.current) return; const result = await query.refetch(); if (result.data && !result.isError) apply(result.data); };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault(); if (lock.current || disabled || !snapshot || !validLimit || !dirty || (restricting && !confirmed)) return;
    lock.current = true; setBusy(true); setError("");
    try {
      const saved = await api.postgresRoleAccessSave(version, { oid: snapshot.oid, name: snapshot.name, canLogin, connectionLimit: nextLimit, revision: snapshot.revision, confirmRestriction: confirmed });
      queryClient.setQueryData(queryKey, saved);
      toast.success(t("pgAccess.saved")); onSaved(); onClose();
    } catch (cause) { const parsed = normalizeError(cause); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); setNeedsReload(true); }
    finally { lock.current = false; setBusy(false); }
  };
  return <Dialog open onOpenChange={(open) => !open && close()}><DialogContent hideClose={busy} className="flex max-w-xl max-h-[85dvh] flex-col overflow-hidden">
    <DialogHeader><DialogTitle className="pr-6">{t("pgAccess.title")}</DialogTitle><DialogDescription className="break-words">PostgreSQL {version} · 127.0.0.1:{port}<span className="mt-1 block break-all font-mono text-foreground">{target.name}</span></DialogDescription></DialogHeader>
    <form onSubmit={submit} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
      {changed && <p role="alert" className="text-sm text-error">{t("db.pgChanged")}</p>}
      {query.isPending && <p role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 animate-spin" />{t("db.loading")}</p>}
      {query.isError && <p role="alert" className="break-words text-sm text-error">{normalizeError(query.error).message}</p>}
      {snapshot && <>
        <p className="text-xs leading-5 text-muted">{t("pgAccess.description")}</p>
        {snapshot.protected && <p className="rounded-md bg-warn-soft p-3 text-sm text-warn">{t("pgAccess.protected")}</p>}
        <div className="space-y-1.5"><Label htmlFor="pg-access-login">{t("pgAccess.login")}</Label><Select value={canLogin ? "allow" : "pause"} disabled={disabled} onValueChange={(value) => { setCanLogin(value === "allow"); setConfirmed(false); }}><SelectTrigger id="pg-access-login"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="allow">{t("pgAccess.allow")}</SelectItem><SelectItem value="pause">{t("pgAccess.pause")}</SelectItem></SelectContent></Select><p className="text-xs leading-5 text-muted">{t("pgAccess.loginHint")}</p></div>
        <div className="space-y-3 border-t border-dashed border-border pt-4"><div className="space-y-1.5"><Label htmlFor="pg-access-limit-mode">{t("pgAccess.limit")}</Label><Select value={unlimited ? "unlimited" : "custom"} disabled={disabled} onValueChange={(value) => { setUnlimited(value === "unlimited"); setConfirmed(false); }}><SelectTrigger id="pg-access-limit-mode"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="unlimited">{t("pgAccess.unlimited")}</SelectItem><SelectItem value="custom">{t("pgAccess.custom")}</SelectItem></SelectContent></Select></div>
          {!unlimited && <div className="space-y-1.5"><Label htmlFor="pg-access-limit">{t("pgAccess.maxConnections")}</Label><Input id="pg-access-limit" type="number" inputMode="numeric" min={0} max={2147483647} step={1} value={limit} disabled={disabled} aria-invalid={!validLimit} aria-describedby="pg-access-limit-help" onChange={(event) => { setLimit(event.target.value); setConfirmed(false); }} /><p id="pg-access-limit-help" className={`text-xs ${validLimit ? "text-muted" : "text-error"}`}>{t("pgAccess.limitHint")}</p></div>}
          <p className="text-xs leading-5 text-muted">{t("pgAccess.limitScope")}</p>
          <p className="text-xs text-secondary">{t("pgAccess.connections")}: <span className="font-mono">{snapshot.activeConnections}</span></p>
          {(!canLogin || nextLimit === 0) && <p className="text-xs leading-5 text-warn">{t("pgAccess.existingHint")}</p>}
        </div>
        {restricting && <label className="flex items-start gap-2 border-t border-dashed border-border pt-4 text-sm leading-5"><input type="checkbox" className="mt-1 h-4 w-4 shrink-0 accent-primary" checked={confirmed} disabled={disabled} onChange={(event) => setConfirmed(event.target.checked)} /><span>{t("pgAccess.confirm")}</span></label>}
      </>}
      {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
      {!changed && <Button type="button" variant="ghost" size="sm" className="h-auto max-w-full whitespace-normal text-left" disabled={busy || query.isFetching} onClick={reload}>{query.isFetching && <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" />}{t("pgAccess.reload")}</Button>}
    </div><DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={close}>{t("common.cancel")}</Button>{!snapshot?.protected && <Button type="submit" disabled={disabled || !validLimit || !dirty || (restricting && !confirmed)}>{busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{t("pg.save")}</Button>}</DialogFooter></form>
  </DialogContent></Dialog>;
}

function Pager({ page, pages, onChange }: { page: number; pages: number; onChange: (page: number) => void }) {
  const t = useT();
  return <div className="mt-3 flex flex-wrap items-center justify-end gap-2"><Button variant="ghost" size="sm" disabled={page <= 1} onClick={() => onChange(page - 1)}>{t("pg.previous")}</Button><span className="text-xs text-muted">{page} / {pages}</span><Button variant="ghost" size="sm" disabled={page >= pages} onClick={() => onChange(page + 1)}>{t("pg.next")}</Button></div>;
}
