"use client";

import * as React from "react";
import { toast } from "sonner";
import { Download, Loader2 } from "lucide-react";
import { useT } from "@/lib/store";
import { useInvalidate, toastError } from "@/lib/hooks";
import { normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { fmtBytes } from "@/lib/utils";

/** 检测来源 → 选择业务库 → 确认目标 → 完整导出、保护性备份与还原。 */
export function DbImportDialog({ open, onOpenChange, version, targetLabel }: {
  open: boolean; onOpenChange: (open: boolean) => void; version: string; targetLabel: string;
}) {
  const t = useT();
  const invalidate = useInvalidate();
  const [source, setSource] = React.useState({ host: "127.0.0.1", port: "3306", user: "root", password: "" });
  const [phase, setPhase] = React.useState<"idle" | "probing" | "importing">("idle");
  const busyRef = React.useRef(false);
  const busy = phase !== "idle";
  const [dbs, setDbs] = React.useState<api.SourceDb[] | null>(null);
  const [selected, setSelected] = React.useState<Set<string>>(new Set());
  const [confirmed, setConfirmed] = React.useState(false);
  const [error, setError] = React.useState("");
  const [report, setReport] = React.useState<api.ImportReport | null>(null);
  const valid = source.host.trim().length > 0 && source.user.trim().length > 0 && /^\d+$/.test(source.port) && Number(source.port) >= 1 && Number(source.port) <= 65535;
  const change = (field: keyof typeof source, value: string) => {
    if (busyRef.current) return;
    setSource((current) => ({ ...current, [field]: value }));
    setDbs(null); setSelected(new Set()); setConfirmed(false); setReport(null); setError("");
  };
  const close = (value: boolean) => {
    if (busyRef.current) return;
    if (!value) { setSource((current) => ({ ...current, password: "" })); setDbs(null); setSelected(new Set()); setConfirmed(false); setReport(null); setError(""); }
    onOpenChange(value);
  };
  const probe = async () => {
    if (busyRef.current || !valid || !version) return;
    busyRef.current = true; setPhase("probing"); setDbs(null); setSelected(new Set()); setError(""); setReport(null); setConfirmed(false);
    try {
      const list = await api.migrateListSource(source.host.trim(), Number(source.port), source.user.trim(), source.password, version);
      setDbs(list);
    } catch (error) { setError(normalizeError(error).message); toastError(error); }
    finally { busyRef.current = false; setPhase("idle"); }
  };
  const runImport = async () => {
    if (busyRef.current || !confirmed || !dbs || !valid || !selected.size || !version) return;
    busyRef.current = true; setPhase("importing"); setError(""); setReport(null);
    try {
      const result = await api.migrateImport(source.host.trim(), Number(source.port), source.user.trim(), source.password, [...selected], version);
      setReport(result); setConfirmed(false);
      if (!result.failed.length) {
        toast.success(`${t("dbImport.doneP1")} ${result.imported.length} ${t("dbImport.doneP2")}`);
        setSelected(new Set());
      } else {
        toast.warning(t("dbImport.partial"));
        setSelected(new Set(result.failed.map(([name]) => name)));
      }
    } catch (error) { setError(normalizeError(error).message); toastError(error); }
    finally { invalidate("databases", "db-users", "db-backups"); busyRef.current = false; setPhase("idle"); }
  };
  return <Dialog open={open} onOpenChange={close}>
    <DialogContent className="flex max-w-xl max-h-[calc(100dvh-2rem)] flex-col overflow-hidden p-4 sm:p-6" hideClose={busy}>
      <DialogHeader><DialogTitle>{t("dbImport.title")}</DialogTitle><DialogDescription>{t("dbImport.desc")}</DialogDescription></DialogHeader>
      <div className="min-h-0 space-y-4 overflow-y-auto pr-1">
      <form className="space-y-3" onSubmit={(event) => { event.preventDefault(); void probe(); }}>
        <div className="grid grid-cols-[minmax(0,1fr)_6rem] gap-3">
          <div className="space-y-1.5"><Label htmlFor="source-host">{t("dbImport.host")}</Label><Input id="source-host" disabled={busy} value={source.host} onChange={(e) => change("host", e.target.value)} /></div>
          <div className="space-y-1.5"><Label htmlFor="source-port">{t("dbImport.port")}</Label><Input id="source-port" inputMode="numeric" disabled={busy} value={source.port} onChange={(e) => change("port", e.target.value)} /></div>
        </div>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5"><Label htmlFor="source-user">{t("dbImport.user")}</Label><Input id="source-user" disabled={busy} value={source.user} onChange={(e) => change("user", e.target.value)} autoComplete="off" /></div>
          <div className="space-y-1.5"><Label htmlFor="source-password">{t("dbImport.password")}</Label><Input id="source-password" type="password" disabled={busy} value={source.password} onChange={(e) => change("password", e.target.value)} autoComplete="current-password" /></div>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-2"><p className="text-xs text-muted">{t("dbImport.sourceChanged")}</p><Button type="submit" variant="secondary" disabled={busy || !valid}>{phase === "probing" && <Loader2 className="h-3.5 w-3.5 animate-spin" />}{t("dbImport.detect")}</Button></div>
      </form>
      {error && <p role="alert" className="break-words text-sm text-error">{error}</p>}
      {dbs && <div className="max-h-48 overflow-y-auto rounded-lg border border-border">
        {!dbs.length ? <p className="p-4 text-sm text-muted">{t("dbImport.none")}</p> : dbs.map((db) => <label key={db.name} className="flex min-w-0 items-center gap-3 px-3 py-2.5">
          <input type="checkbox" disabled={busy} checked={selected.has(db.name)} onChange={(event) => { setSelected((current) => { const next = new Set(current); if (event.target.checked) next.add(db.name); else next.delete(db.name); return next; }); setConfirmed(false); setReport(null); }} />
          <code className="min-w-0 flex-1 break-all text-xs">{db.name}</code>{db.sizeKb != null && <span className="text-xs text-muted">{fmtBytes(db.sizeKb * 1024)}</span>}
        </label>)}
      </div>}
      <div className="space-y-2 rounded-lg bg-fill p-3">
        <p className="break-words text-sm font-medium">{t("db.target")}：{targetLabel}</p>
        <p className="text-xs text-muted">{t("dbImport.readyHint")}</p>
        <label className="flex items-start gap-2 text-sm"><input type="checkbox" className="mt-1 shrink-0" disabled={busy || !selected.size} checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} /><span>{t("dbImport.confirm")}</span></label>
      </div>
      {report && <div role="status" className="space-y-2 text-sm">
        {!!report.imported.length && <p>{t("dbImport.doneP1")} {report.imported.length} {t("dbImport.doneP2")} · {report.imported.join(", ")}</p>}
        {report.failed.map(([db, message]) => <p key={db} className="break-words text-error">{db}：{message}</p>)}
      </div>}
      </div>
      <DialogFooter className="shrink-0 flex-wrap">
        <Button variant="ghost" disabled={busy} onClick={() => close(false)}>{t("common.close")}</Button>
        <Button disabled={busy || !dbs || !selected.size || !confirmed || !valid} onClick={() => void runImport()}>{phase === "importing" ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" />}{phase === "importing" ? t("confirm.busy") : t("dbImport.run")}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
