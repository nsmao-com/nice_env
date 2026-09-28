"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import type { DbBackupFile } from "@nsb/schema";
import { ArchiveRestore, DatabaseBackup, FolderOpen, Loader2, Trash2, Upload } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { isTauri, listen, normalizeError } from "@/lib/backend";
import { useInvalidate, toastError } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { fmtBytes } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

type Action = { kind: "dump" | "restore" | "delete"; signature: string; file?: Pick<DbBackupFile, "name" | "path"> };
const validName = (name: string) => /^[A-Za-z0-9_]{1,63}$/.test(name) && !/^pg_/i.test(name) && !/^(postgres|template0|template1)$/i.test(name);

export function PostgresBackupCard({ version, port, signature, ready, databases, roles, onLockChange }: {
  version: string; port?: number | null; signature: string; ready: boolean; databases: api.PostgresDatabaseInfo[]; roles: api.PostgresRoleInfo[]; onLockChange: (locked: boolean) => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const query = useQuery({ queryKey: ["postgres-backups"], queryFn: api.postgresBackupList, retry: false });
  const [action, setAction] = React.useState<Action | null>(null);
  const [databaseOid, setDatabaseOid] = React.useState("");
  const [name, setName] = React.useState("");
  const [owner, setOwner] = React.useState("");
  const [trusted, setTrusted] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [progress, setProgress] = React.useState<api.PostgresBackupProgress | null>(null);
  const [search, setSearch] = React.useState("");
  const [page, setPage] = React.useState(1);
  const lock = React.useRef(false);
  const files = (query.data ?? []).filter((file) => file.name.toLowerCase().includes(search.toLowerCase()));
  const pages = Math.max(1, Math.ceil(files.length / 10));
  const currentPage = Math.min(page, pages);
  const available = databases.filter((db) => !db.protected && db.allowConnections);
  const selected = available.find((db) => String(db.oid) === databaseOid);
  const changed = !!action && action.kind !== "delete" && (!ready || signature !== action.signature);
  const valid = !!action && !changed && (action.kind === "delete" || (action.kind === "dump" ? !!selected : trusted && validName(name) && !databases.some((db) => db.name === name) && roles.some((role) => role.name === owner && role.canLogin)));
  React.useEffect(() => { onLockChange(busy || !!action); return () => onLockChange(false); }, [busy, action, onLockChange]);
  const begin = (kind: Action["kind"], file?: Action["file"]) => {
    setAction({ kind, signature, file }); setError(""); setProgress(null); setTrusted(false); setName("");
    setDatabaseOid(available.length === 1 ? String(available[0].oid) : "");
    setOwner(roles.find((role) => role.canLogin && !role.superuser)?.name ?? roles.find((role) => role.name === "postgres")?.name ?? "");
  };
  const close = () => { if (!lock.current) { setAction(null); setError(""); setProgress(null); } };
  const fail = (error: unknown) => { const parsed = normalizeError(error); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); };
  const pick = async () => {
    if (lock.current || !ready) return;
    if (!isTauri) { toast.info(t("dbBackup.desktopOnly")); return; }
    lock.current = true; setBusy(true); setError("");
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ title: t("pgBackup.pickFile"), multiple: false, directory: false, filters: [{ name: "PostgreSQL custom archive", extensions: ["dump"] }] });
      if (typeof path === "string") begin("restore", { path, name: path.split(/[\\/]/).pop() ?? path });
    } catch (error) { fail(error); }
    finally { lock.current = false; setBusy(false); }
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault(); if (lock.current || !action || !valid) return;
    lock.current = true; setBusy(true); setError(""); setProgress(null);
    const operationId = crypto.randomUUID();
    let stop: (() => void) | undefined;
    try {
      if (action.kind !== "delete") stop = await listen<api.PostgresBackupProgress>("postgres://backup", (value) => { if (value.operationId === operationId) setProgress(value); });
      if (action.kind === "dump" && selected) await api.postgresBackupDump(version, selected.name, selected.oid, operationId);
      else if (action.kind === "restore" && action.file) await api.postgresBackupRestore(version, action.file.path, name, owner, trusted, operationId);
      else if (action.kind === "delete" && action.file) await api.postgresBackupDelete(action.file.name);
      toast.success(t(action.kind === "dump" ? "dbBackup.done" : action.kind === "restore" ? "dbBackup.restored" : "dbBackup.deleted"));
      setAction(null);
    } catch (error) { fail(error); }
    finally { stop?.(); lock.current = false; setBusy(false); setProgress(null); invalidate("postgres-backups", "postgres-databases", "postgres-roles", "postgres-connection"); }
  };
  const title = t(action?.kind === "dump" ? "dbBackup.new" : action?.kind === "restore" ? "pgBackup.restoreTitle" : "dbBackup.deleteTitle");
  return <>
    <Card className="min-w-0"><CardHeader className="flex-row flex-wrap items-start justify-between gap-3">
      <div className="min-w-0"><CardTitle className="flex items-center gap-2 text-sm"><DatabaseBackup className="h-4 w-4 shrink-0" />{t("pgBackup.title")}</CardTitle><CardDescription className="mt-1">{t("pgBackup.subtitle")}</CardDescription></div>
      <div className="flex flex-wrap gap-2">
        <Button size="icon-sm" variant="ghost" aria-label={t("dbBackup.openDir")} disabled={busy} onClick={() => void api.postgresBackupDir().then(api.openInFolder).catch(toastError)}><FolderOpen className="h-4 w-4" /></Button>
        <Button size="sm" variant="secondary" disabled={!ready || busy} onClick={() => void pick()}><Upload className="h-3.5 w-3.5" />{t("pgBackup.pickFile")}</Button>
        <Button size="sm" variant="secondary" disabled={!ready || busy || !available.length} onClick={() => begin("dump")}>{t("dbBackup.new")}</Button>
      </div>
    </CardHeader><CardContent>
      <div className="mb-3 flex gap-2"><Input aria-label={t("pgBackup.search")} placeholder={t("pgBackup.search")} value={search} onChange={(event) => { setSearch(event.target.value); setPage(1); }} /><Button variant="ghost" size="sm" disabled={busy || query.isFetching} onClick={() => void query.refetch()}>{t("db.refresh")}</Button></div>
      {query.isPending ? <p role="status" className="py-4 text-xs text-muted">{t("db.loading")}</p> : query.isError ? <p role="alert" className="py-4 text-sm text-error">{normalizeError(query.error).message}</p> : !files.length ? <p className="py-4 text-xs text-muted">{t(search ? "pg.noMatches" : "dbBackup.empty")}</p> : <div className="divide-y divide-dashed divide-border">
        {files.slice((currentPage - 1) * 10, currentPage * 10).map((file) => <div key={file.path} className="flex flex-wrap items-center gap-2 py-3">
          <div className="min-w-0 flex-1 basis-40"><p className="truncate font-mono text-xs" title={file.name}>{file.name}</p><p className="mt-1 text-xs text-muted">{fmtBytes(file.sizeBytes)} · {file.createdAt ? new Date(file.createdAt * 1000).toLocaleString() : "—"}</p></div>
          <Button size="icon-sm" variant="ghost" disabled={!ready || busy} aria-label={`${t("dbBackup.restore")} ${file.name}`} onClick={() => begin("restore", file)}><ArchiveRestore className="h-4 w-4" /></Button>
          <Button size="icon-sm" variant="ghost" disabled={busy} className="text-faint hover:text-error" aria-label={`${t("dbBackup.delete")} ${file.name}`} onClick={() => begin("delete", file)}><Trash2 className="h-4 w-4" /></Button>
        </div>)}
      </div>}
      {pages > 1 && <div className="mt-3 flex flex-wrap items-center justify-end gap-2"><Button size="sm" variant="ghost" disabled={currentPage === 1} onClick={() => setPage(currentPage - 1)}>{t("pg.previous")}</Button><span className="text-xs text-muted">{currentPage} / {pages}</span><Button size="sm" variant="ghost" disabled={currentPage === pages} onClick={() => setPage(currentPage + 1)}>{t("pg.next")}</Button></div>}
      {error && !action && <p role="alert" className="mt-3 break-words text-sm text-error">{error}</p>}
    </CardContent></Card>
    <Dialog open={!!action} onOpenChange={(open) => !open && close()}><DialogContent hideClose={busy} className="flex max-w-xl max-h-[85dvh] flex-col overflow-hidden">
      <DialogHeader><DialogTitle className="pr-6 break-words">{title}</DialogTitle><DialogDescription>{action?.kind === "delete" ? t("pgBackup.deleteHint") : `PostgreSQL ${version} · 127.0.0.1:${port ?? "—"}`}</DialogDescription></DialogHeader>
      <form onSubmit={submit} className="flex min-h-0 flex-col gap-4"><div className="min-h-0 space-y-4 overflow-y-auto px-0.5">
        {changed && <p role="alert" className="text-sm text-error">{t("db.pgChanged")}</p>}
        {action?.file && <p className="break-all rounded-xl bg-fill px-3 py-2 font-mono text-xs">{action.file.path}</p>}
        {action?.kind === "dump" && <div className="space-y-1.5"><Label htmlFor="pg-backup-database">{t("pg.databaseName")}</Label><Select value={databaseOid} disabled={busy || changed} onValueChange={setDatabaseOid}><SelectTrigger id="pg-backup-database"><SelectValue placeholder={t("pgBackup.chooseDatabase")} /></SelectTrigger><SelectContent>{available.map((db) => <SelectItem key={db.oid} value={String(db.oid)}>{db.name}</SelectItem>)}</SelectContent></Select><p className="text-xs text-muted">{t("pgBackup.dumpHint")}</p></div>}
        {action?.kind === "restore" && <>
          <p className="text-xs text-muted">{t("pgBackup.restoreHint")}</p>
          <div className="space-y-1.5"><Label htmlFor="pg-restore-name">{t("pgBackup.newName")}</Label><Input id="pg-restore-name" value={name} disabled={busy || changed} maxLength={63} autoComplete="off" onChange={(event) => setName(event.target.value)} /><p className={`text-xs ${name && (!validName(name) || databases.some((db) => db.name === name)) ? "text-error" : "text-muted"}`}>{t(databases.some((db) => db.name === name) ? "pgBackup.nameExists" : "pg.nameHint")}</p></div>
          <div className="space-y-1.5"><Label htmlFor="pg-restore-owner">{t("pg.owner")}</Label><Select value={owner} disabled={busy || changed} onValueChange={setOwner}><SelectTrigger id="pg-restore-owner"><SelectValue placeholder={t("pg.chooseOwner")} /></SelectTrigger><SelectContent>{roles.filter((role) => role.canLogin).map((role) => <SelectItem key={role.oid} value={role.name}>{role.name}</SelectItem>)}</SelectContent></Select></div>
          <label className="flex items-start gap-3 text-sm"><input type="checkbox" checked={trusted} disabled={busy || changed} onChange={(event) => setTrusted(event.target.checked)} className="mt-0.5 h-4 w-4 shrink-0 accent-[var(--primary)]" /><span>{t("pgBackup.trust")}</span></label>
        </>}
        {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
        {busy && <p role="status" className="flex items-center gap-2 text-sm text-muted"><Loader2 className="h-4 w-4 shrink-0 animate-spin" />{action?.kind === "dump" ? `${t("pgBackup.exporting")} · ${fmtBytes(progress?.bytes ?? 0)}` : t(action?.kind === "restore" ? "pgBackup.restoring" : "confirm.busy")}</p>}
      </div><DialogFooter className="shrink-0 flex-col-reverse sm:flex-row"><Button type="button" variant="ghost" disabled={busy} onClick={close}>{t("common.cancel")}</Button><Button type="submit" variant={action?.kind === "delete" ? "destructive" : "default"} disabled={busy || !valid}>{t(action?.kind === "delete" ? "dbBackup.delete" : action?.kind === "restore" ? "dbBackup.restore" : "pgBackup.export")}</Button></DialogFooter></form>
    </DialogContent></Dialog>
  </>;
}
