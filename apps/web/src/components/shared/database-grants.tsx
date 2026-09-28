"use client";

import * as React from "react";
import type { DatabaseEngine, DbUserInfo } from "@nsb/schema";
import { useQuery } from "@tanstack/react-query";
import { toast } from "sonner";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useInvalidate } from "@/lib/hooks";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetContent, SheetHeader, SheetTitle, SheetDescription } from "@/components/ui/sheet";

const dataPrivileges = ["SELECT", "INSERT", "UPDATE", "DELETE"];
const structurePrivileges = ["CREATE", "ALTER", "INDEX", "DROP", "CREATE VIEW", "SHOW VIEW"];
const privilegeLabels: Record<string, string> = {
  SELECT: "select", INSERT: "insert", UPDATE: "update", DELETE: "delete", CREATE: "create", ALTER: "alter", INDEX: "index", DROP: "drop",
  "CREATE VIEW": "createView", "SHOW VIEW": "showView", REFERENCES: "references", "CREATE TEMPORARY TABLES": "temporary", "LOCK TABLES": "lock",
  "CREATE ROUTINE": "createRoutine", "ALTER ROUTINE": "alterRoutine", EXECUTE: "execute", EVENT: "event", TRIGGER: "trigger", "DELETE HISTORY": "history",
};

export function DatabaseGrantsSheet({ engine, version, account, targetLabel, ready, onClose }: {
  engine: DatabaseEngine; version: string; account: DbUserInfo; targetLabel: string; ready: boolean; onClose: () => void;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const id = React.useId();
  const query = useQuery({ queryKey: ["db-grants", engine, version, account.username, account.host], queryFn: () => api.dbGrants(engine, version, account.username, account.host), retry: false, refetchOnWindowFocus: false, staleTime: 0 });
  const [snapshot, setSnapshot] = React.useState<api.DatabaseGrants | null>(null);
  const [selection, setSelection] = React.useState("");
  const [privileges, setPrivileges] = React.useState<string[]>([]);
  const [grantOption, setGrantOption] = React.useState(false);
  const [confirmed, setConfirmed] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const lock = React.useRef(false);
  const [error, setError] = React.useState("");
  const [needsReload, setNeedsReload] = React.useState(false);
  const [advanced, setAdvanced] = React.useState(false);
  const choose = React.useCallback((value: string, data: api.DatabaseGrants) => {
    const scope = value.startsWith("scope:") ? data.scopes.find((item) => item.scope === value.slice(6)) : undefined;
    setSelection(value); setPrivileges(scope?.privileges ?? []); setGrantOption(scope?.grantOption ?? false); setConfirmed(false); setError("");
    setAdvanced(!!scope?.grantOption || !!scope?.privileges.some((name) => !dataPrivileges.includes(name) && !structurePrivileges.includes(name)));
  }, []);
  React.useEffect(() => {
    if (query.data && !query.isFetching && !snapshot) {
      setSnapshot(query.data);
      choose(query.data.scopes.length ? `scope:${query.data.scopes[0].scope}` : query.data.databases.length ? `db:${query.data.databases[0]}` : "", query.data);
    }
  }, [query.data, query.isFetching, snapshot, choose]);
  const scope = snapshot?.scopes.find((item) => `scope:${item.scope}` === selection);
  const protectedScope = !!snapshot?.protected || !!scope?.protected || !!(snapshot?.partialRevokes && snapshot.globalPrivileges);
  const disabled = busy || !ready || protectedScope || needsReload;
  const removed = (scope?.privileges ?? []).filter((name) => !privileges.includes(name));
  const reducing = removed.length > 0 || (!!scope?.grantOption && !grantOption);
  const changed = [...privileges].sort().join(",") !== [...(scope?.privileges ?? [])].sort().join(",") || grantOption !== !!scope?.grantOption;
  const preset = !privileges.length ? "none" : privileges.length === 1 && privileges[0] === "SELECT" ? "read" : privileges.length === 4 && dataPrivileges.every((name) => privileges.includes(name)) ? "write" : "custom";
  const fail = (cause: unknown) => { const parsed = normalizeError(cause); setError([parsed.message, parsed.hint, parsed.detail].filter(Boolean).join(" ")); };
  const reload = async () => {
    if (lock.current) return;
    lock.current = true; setBusy(true);
    try {
      const data = await api.dbGrants(engine, version, account.username, account.host);
      setSnapshot(data); setNeedsReload(false);
      const selectedStillExists = data.scopes.some((item) => `scope:${item.scope}` === selection) || data.databases.some((name) => `db:${name}` === selection);
      choose(selectedStillExists ? selection : data.scopes.length ? `scope:${data.scopes[0].scope}` : data.databases.length ? `db:${data.databases[0]}` : "", data);
    } catch (cause) { fail(cause); }
    finally { lock.current = false; setBusy(false); }
  };
  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    if (lock.current || disabled || !snapshot || !selection || !changed || (reducing && !confirmed)) return;
    lock.current = true; setBusy(true); setError("");
    try {
      await api.dbGrantsSave(engine, version, { username: account.username, host: account.host, target: selection.slice(selection.startsWith("scope:") ? 6 : 3), newDatabase: selection.startsWith("db:"), privileges, grantOption, revision: snapshot.revision });
      toast.success(t("dbGrants.saved")); invalidate("db-users", "db-grants"); onClose();
    } catch (cause) { fail(cause); setNeedsReload(true); }
    finally { lock.current = false; setBusy(false); }
  };
  const toggle = (name: string) => { setPrivileges((current) => current.includes(name) ? current.filter((value) => value !== name) : [...current, name]); setConfirmed(false); };
  const checks = (names: string[]) => <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">{names.filter((name) => snapshot?.available.includes(name)).map((name) => <label key={name} className="flex min-w-0 items-start gap-2 rounded-lg bg-fill px-3 py-2 text-sm"><input type="checkbox" className="mt-1 h-4 w-4 shrink-0 accent-[var(--primary)]" checked={privileges.includes(name)} disabled={disabled} onChange={() => toggle(name)} /><span className="min-w-0 break-words">{t(`dbGrants.priv.${privilegeLabels[name]}` as "dbGrants.priv.select")}<span className="block break-words text-[10px] text-faint">{name}</span></span></label>)}</div>;
  return <Sheet open onOpenChange={(open) => !open && !lock.current && onClose()}>
    <SheetContent className="flex w-[calc(100%-24px)] flex-col overflow-hidden sm:w-[680px] sm:max-w-[680px]">
      <SheetHeader className="shrink-0 pr-12"><SheetTitle>{t("dbGrants.title")}</SheetTitle><SheetDescription className="break-words">{targetLabel}<span className="mt-1 block break-all font-mono">{account.username || t("dbGrants.anonymous")} @ {account.host}</span></SheetDescription></SheetHeader>
      <form onSubmit={save} className="flex min-h-0 flex-1 flex-col">
        <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-6 pb-5">
          {!snapshot ? <p role={query.isError ? "alert" : "status"} className="text-sm text-muted">{query.isError ? normalizeError(query.error).message : t("common.loading")}{query.isError && <Button type="button" variant="ghost" onClick={() => void query.refetch()}>{t("db.retry")}</Button>}</p> : <>
            <p className="text-xs leading-5 text-muted">{t("dbGrants.scopeHint")}</p>
            {snapshot.partialRevokes && <p className="text-xs leading-5 text-muted">{t("dbGrants.literalMode")}</p>}
            {snapshot.globalPrivileges && <p className="rounded-lg bg-warning-soft p-3 text-xs">{t("dbGrants.global")}</p>}
            {!ready && <p role="alert" className="text-sm text-error">{t("db.serviceStoppedHint")}</p>}
            {protectedScope && <p role="status" className="rounded-lg bg-warning-soft p-3 text-xs">{t("dbGrants.protected")}</p>}
            <div className="space-y-2"><Label htmlFor={`${id}-scope`}>{t("dbGrants.scope")}</Label><Select value={selection} disabled={busy || needsReload} onValueChange={(value) => choose(value, snapshot)}><SelectTrigger id={`${id}-scope`}><SelectValue placeholder={t("db.selectDatabase")} /></SelectTrigger><SelectContent>
              {snapshot.scopes.map((item) => <SelectItem key={`scope:${item.scope}`} value={`scope:${item.scope}`}>{item.label} · {t(item.pattern ? "dbGrants.pattern" : "dbGrants.existing")}</SelectItem>)}
              {snapshot.databases.filter((name) => !snapshot.scopes.some((item) => !item.pattern && item.label === name)).map((name) => <SelectItem key={`db:${name}`} value={`db:${name}`}>{name} · {t("dbGrants.new")}</SelectItem>)}
            </SelectContent></Select><p className="text-xs text-muted">{t("dbGrants.switchHint")}</p></div>
            {scope?.pattern && <p role="status" className="rounded-lg bg-warning-soft p-3 text-xs leading-5">{t("dbGrants.patternHint")}</p>}
            {selection ? <>
              <div className="space-y-2"><Label htmlFor={`${id}-preset`}>{t("dbGrants.preset")}</Label><Select value={preset} disabled={disabled} onValueChange={(value) => { if (value === "custom") return; setPrivileges(value === "read" ? ["SELECT"] : value === "write" ? [...dataPrivileges] : []); setConfirmed(false); }}><SelectTrigger id={`${id}-preset`}><SelectValue /></SelectTrigger><SelectContent>{["read", "write", "none", "custom"].map((value) => <SelectItem key={value} value={value}>{t(`dbGrants.${value}` as "dbGrants.read")}</SelectItem>)}</SelectContent></Select></div>
              <fieldset className="space-y-2"><legend className="mb-2 text-sm font-medium">{t("dbGrants.data")}</legend>{checks(dataPrivileges)}</fieldset>
              <fieldset className="space-y-2 border-t border-dashed border-border pt-4"><legend className="text-sm font-medium">{t("dbGrants.structure")}</legend>{checks(structurePrivileges)}</fieldset>
              <details open={advanced} onToggle={(event) => setAdvanced(event.currentTarget.open)} className="space-y-3 border-t border-dashed border-border pt-4"><summary className="cursor-pointer text-sm font-medium">{t("dbGrants.advanced")}</summary>{checks(snapshot.available.filter((name) => !dataPrivileges.includes(name) && !structurePrivileges.includes(name)))}
                <label className="flex items-start gap-2 text-sm"><input type="checkbox" className="mt-1 h-4 w-4 shrink-0 accent-[var(--primary)]" checked={grantOption} disabled={disabled} onChange={(event) => { setGrantOption(event.target.checked); setConfirmed(false); }} /><span>{t("dbGrants.delegate")}<span className="mt-1 block text-xs leading-5 text-muted">{t("dbGrants.delegateHint")}</span></span></label>
              </details>
              {!!scope?.extraPrivileges.length && <p className="break-words text-xs text-muted">{t("dbGrants.extra")}: {scope.extraPrivileges.join(", ")}</p>}
              {reducing && <label className="flex items-start gap-2 rounded-lg bg-warning-soft p-3 text-sm"><input type="checkbox" checked={confirmed} disabled={disabled} onChange={(event) => setConfirmed(event.target.checked)} className="mt-1 h-4 w-4 shrink-0 accent-[var(--primary)]" /><span>{t("dbGrants.confirmReduction")}</span></label>}
            </> : <p className="text-sm text-muted">{t("dbGrants.empty")}</p>}
          </>}
          {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
          {needsReload && <Button type="button" variant="secondary" disabled={busy} onClick={() => void reload()}>{t("dbGrants.reload")}</Button>}
        </div>
        <div className="mx-6 flex shrink-0 flex-col-reverse gap-2 border-t border-dashed border-border py-4 sm:flex-row sm:justify-end"><Button type="button" variant="ghost" disabled={busy} onClick={onClose}>{t("common.cancel")}</Button><Button type="submit" disabled={disabled || !snapshot || !selection || !changed || (reducing && !confirmed)}>{busy ? t("confirm.busy") : t("dbGrants.save")}</Button></div>
      </form>
    </SheetContent>
  </Sheet>;
}
