"use client";

import * as React from "react";
import { toast } from "sonner";
import { Database, HardDrive, KeyRound, Plus, Table2, Trash2, UserRound, ExternalLink } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { useDatabases, useDbUsers, useInvalidate, toastError, usePorts } from "@/lib/hooks";
import * as api from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { DbBackupCard } from "@/components/shared/db-backup";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/misc";
import { ServiceCard } from "@/components/shared/service-card";
import { CopyButton, SectionHeader, ConfirmDialog } from "@/components/shared/misc";
import { StatChip } from "@/components/shared/stat-chip";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from "@/components/ui/dialog";
import { PageHeader } from "@/components/layout/app-shell";

export default function DatabasesPage() {
  const t = useT();
  const ports = usePorts();
  const invalidate = useInvalidate();
  const { data: dbs } = useDatabases();
  const { data: users } = useDbUsers();
  const [createOpen, setCreateOpen] = React.useState(false);
  const [userOpen, setUserOpen] = React.useState(false);
  const [rootOpen, setRootOpen] = React.useState(false);
  const [dropTarget, setDropTarget] = React.useState<string | null>(null);
  const [dropping, setDropping] = React.useState(false);
  const [dropTyped, setDropTyped] = React.useState("");

  const systemDbs = new Set(["mysql", "sys", "information_schema", "performance_schema"]);

  return (
    <div className="pb-8">
      <PageHeader
        title={t("db.title")}
        subtitle={t("db.subtitle")}
        actions={
          <>
            <Button variant="secondary" onClick={() => setRootOpen(true)}>
              <KeyRound className="h-3.5 w-3.5" /> {t("db.resetRoot")}
            </Button>
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className="h-3.5 w-3.5" /> {t("db.createDb")}
            </Button>
          </>
        }
      />

      {/* 实例卡片：MySQL + Redis */}
      <section className="mb-6">
        <SectionHeader title={t("db.instance")} className="mb-3" />
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          <MySqlInstanceCard />
          <RedisInstanceCard />
        </div>
      </section>

      {/* 备份 / 还原 */}
      <section className="mb-6">
        <DbBackupCard />
      </section>

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-3">
        {/* 数据库列表 */}
        <Card className="lg:col-span-2">
          <CardHeader className="flex-row items-center justify-between">
            <CardTitle className="flex items-center gap-2 text-[13px]">
              <Database className="h-3.5 w-3.5 text-primary" /> {t("db.databases")}
            </CardTitle>
            <Button variant="ghost" size="sm" onClick={() => setUserOpen(true)}>
              <UserRound className="h-3.5 w-3.5" /> {t("db.createUser")}
            </Button>
          </CardHeader>
          <CardContent>
            {dbs.length === 0 ? (
              <p className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-xs text-faint">
                {t("db.emptyHint")}
              </p>
            ) : (
              <div className="flex flex-col divide-y divide-border">
                {dbs.map((db) => (
                  <div key={db.name} className="flex items-center gap-3 py-2.5">
                    <Table2 className="h-3.5 w-3.5 shrink-0 text-faint" />
                    <span className="flex-1 truncate font-mono text-[12.5px]">{db.name}</span>
                    {db.tables != null && <StatChip icon={Table2}>{db.tables} {t("db.tables")}</StatChip>}
                    {systemDbs.has(db.name) && <Badge variant="muted">{t("db.systemDb")}</Badge>}
                    <CopyButton text={`mysql://root@127.0.0.1:${ports.mysql}/${db.name}`} />
                    {/* 系统库不给删：删了 MySQL 直接起不来 */}
                    {!systemDbs.has(db.name) && (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        title={t("db.drop")}
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
            {users.length === 0 ? (
              <p className="text-xs text-faint">{t("db.noUsers")}</p>
            ) : (
              users.map((u) => (
                <div key={`${u.username}@${u.host}`} className="rounded-lg border border-border bg-card-2/40 px-3 py-2">
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

      <CreateDbDialog open={createOpen} onOpenChange={setCreateOpen} onDone={() => invalidate("databases")} />
      <CreateUserDialog open={userOpen} onOpenChange={setUserOpen} onDone={() => invalidate("db-users")} />
      <ResetRootDialog open={rootOpen} onOpenChange={setRootOpen} />

      {/* 删库不可恢复：要求用户把库名完整敲一遍才允许执行 */}
      <ConfirmDialog
        open={dropTarget !== null}
        onOpenChange={(o) => !o && setDropTarget(null)}
        title={`${t("confirm.deleteDb")} · ${dropTarget ?? ""}`}
        description={t("confirm.deleteDbDesc").replace("{name}", dropTarget ?? "")}
        confirmText={t("db.drop")}
        danger
        loading={dropping}
        onConfirm={async () => {
          if (!dropTarget || dropTyped !== dropTarget) return;
          setDropping(true);
          try {
            await api.dbDrop(dropTarget);
            toast.success(`${t("db.droppedP1")} ${dropTarget} ${t("db.droppedP2")}`);
            invalidate("databases");
          } catch (e) {
            toastError(e);
          } finally {
            setDropping(false);
            setDropTarget(null);
          }
        }}
      >
        <div className="flex flex-col gap-2">
          <Label className="text-[11.5px] text-faint">
            {t("confirm.deleteDbPrompt").replace("{name}", dropTarget ?? "")}
          </Label>
          <Input
            value={dropTyped}
            onChange={(e) => setDropTyped(e.target.value)}
            placeholder={dropTarget ?? ""}
            className="font-mono text-[12px]"
            autoFocus
          />
          {dropTyped.length > 0 && dropTyped !== dropTarget && (
            <p className="text-[10.5px] text-error">{t("confirm.mismatch")}</p>
          )}
        </div>
      </ConfirmDialog>
    </div>
  );
}

function MySqlInstanceCard() {
  const t = useT();
  const ports = usePorts();
  const { data: dbs } = useDatabases();
  return (
    <Card className="p-4">
      <div className="mb-3 flex items-center gap-2">
        <div className="flex h-9 w-9 items-center justify-center rounded-lg border border-border bg-card-2/60">
          <Database className="h-4 w-4 text-info" strokeWidth={1.8} />
        </div>
        <div>
          <p className="text-[13px] font-medium">MySQL</p>
          <p className="text-[11px] text-faint">127.0.0.1:{ports.mysql} · utf8mb4</p>
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-1.5">
        <StatChip icon={Database}>{dbs.length} {t("db.dbs")}</StatChip>
        <StatChip icon={HardDrive}>{t("db.datadir")} · {t("db.datadirSuffix")}</StatChip>
      </div>
      <Separator className="my-3" />
      <div className="flex flex-col gap-1.5">
        <p className="text-[11px] font-medium text-secondary">{t("db.connStrings")}</p>
        {[
          ["URL", `mysql://root@127.0.0.1:${ports.mysql}/dbname`],
          ["CLI", `mysql -u root -h 127.0.0.1 -P ${ports.mysql}`],
          ["PDO", `"mysql:host=127.0.0.1;port=${ports.mysql};dbname=dbname"`],
        ].map(([k, v]) => (
          <div key={k} className="flex items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5">
            <span className="w-7 shrink-0 text-[10px] font-medium text-faint">{k}</span>
            <code className="flex-1 truncate font-mono text-[11px] text-secondary">{v}</code>
            <CopyButton text={v} />
          </div>
        ))}
      </div>
      <div className="mt-3 flex gap-2">
        <Button variant="secondary" size="sm" onClick={() => api.openInBrowser(`http://127.0.0.1:${ports.http}/_adminer/`).catch(toastError)}>
          <ExternalLink className="h-3.5 w-3.5" /> {t("db.openAdminer")}
        </Button>
      </div>
    </Card>
  );
}

function RedisInstanceCard() {
  const t = useT();
  const ports = usePorts();
  return (
    <Card className="p-4">
      <div className="mb-3 flex items-center gap-2">
        <div className="flex h-9 w-9 items-center justify-center rounded-lg border border-border bg-card-2/60">
          <span className="font-mono text-[13px] font-bold text-error">R</span>
        </div>
        <div>
          <p className="text-[13px] font-medium">Redis</p>
          <p className="text-[11px] text-faint">127.0.0.1:{ports.redis} · {t("db.redisDevHint")}</p>
        </div>
      </div>
      <div className="flex flex-col gap-1.5">
        <p className="text-[11px] font-medium text-secondary">{t("db.connMethod")}</p>
        <div className="flex items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5">
          <span className="w-7 shrink-0 text-[10px] font-medium text-faint">CLI</span>
          <code className="flex-1 truncate font-mono text-[11px] text-secondary">{`redis-cli -p ${ports.redis}`}</code>
          <CopyButton text={`redis-cli -p ${ports.redis}`} />
        </div>
        <div className="flex items-center gap-2 rounded-md bg-card-2/50 px-2.5 py-1.5">
          <span className="w-7 shrink-0 text-[10px] font-medium text-faint">URL</span>
          <code className="flex-1 truncate font-mono text-[11px] text-secondary">{`redis://127.0.0.1:${ports.redis}/0`}</code>
          <CopyButton text={`redis://127.0.0.1:${ports.redis}/0`} />
        </div>
      </div>
    </Card>
  );
}

function CreateDbDialog({ open, onOpenChange, onDone }: { open: boolean; onOpenChange: (o: boolean) => void; onDone: () => void }) {
  const t = useT();
  const [name, setName] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      await api.dbCreate(name);
      toast.success(`${t("db.createdP1")} ${name} ${t("db.createdP2")}`);
      onOpenChange(false);
      setName("");
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle>{t("db.createDb")}</DialogTitle>
          <DialogDescription>CREATE DATABASE · utf8mb4_unicode_ci</DialogDescription>
        </DialogHeader>
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="my_database" className="font-mono" autoFocus />
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={!name || busy}>{t("db.create")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function CreateUserDialog({ open, onOpenChange, onDone }: { open: boolean; onOpenChange: (o: boolean) => void; onDone: () => void }) {
  const t = useT();
  const [form, setForm] = React.useState({ username: "", password: "", database: "" });
  const [busy, setBusy] = React.useState(false);
  const { data: dbs } = useDatabases();
  const submit = async () => {
    setBusy(true);
    try {
      await api.dbCreateUser(form.username, form.password, form.database);
      toast.success(`${t("db.userCreatedP1")} ${form.username} ${t("db.userCreatedP2")}`);
      onOpenChange(false);
      setForm({ username: "", password: "", database: "" });
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle>{t("db.createUser")}</DialogTitle>
          <DialogDescription>{t("db.grantHint")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label>{t("db.username")}</Label>
            <Input value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value })} className="font-mono" />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label>{t("db.password")}</Label>
            <Input value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} className="font-mono" />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label>{t("db.grantDb")}</Label>
            <Input
              value={form.database}
              onChange={(e) => setForm({ ...form, database: e.target.value })}
              placeholder="my_database"
              className="font-mono"
              list="db-list"
            />
            <datalist id="db-list">
              {dbs.map((d) => (
                <option key={d.name} value={d.name} />
              ))}
            </datalist>
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={!form.username || !form.password || !form.database || busy}>{t("db.create")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ResetRootDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const t = useT();
  const [pass, setPass] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      await api.dbResetRootPassword(pass);
      toast.success(t("db.rootUpdated"));
      onOpenChange(false);
      setPass("");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-sm">
        <DialogHeader>
          <DialogTitle>{t("db.resetRoot")}</DialogTitle>
          <DialogDescription>{t("db.secretHint")}</DialogDescription>
        </DialogHeader>
        <Input
          type="text"
          value={pass}
          onChange={(e) => setPass(e.target.value)}
          placeholder={t("db.randomPass")}
          className="font-mono"
        />
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button onClick={submit} disabled={busy}>{t("db.reset")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
