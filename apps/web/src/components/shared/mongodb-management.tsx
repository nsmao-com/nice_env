"use client";

import * as React from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { ChevronLeft, ChevronRight, Database, RefreshCw, Search } from "lucide-react";
import type { MongoFilter, ServiceStatus } from "@nsb/schema";
import * as api from "@/lib/api";
import { normalizeError } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CopyButton } from "@/components/shared/misc";
import { MongoAuthPanel } from "@/components/shared/mongodb-auth";
import { MongoBackupPanel } from "@/components/shared/mongodb-backup";

const options = { retry: (count: number, error: unknown) => count < 2 && normalizeError(error).code === "SERVICE_BUSY", retryDelay: 700, refetchOnWindowFocus: false };
const PAGE_SIZE = 10;

function QueryError({ error, busy, retry }: { error: unknown; busy: boolean; retry: () => void }) {
  const t = useT(); const parsed = normalizeError(error);
  return <div className="space-y-2"><p role="alert" className="break-words text-sm text-error">{parsed.message}</p>{parsed.hint && <p className="break-words text-xs text-muted">{parsed.hint}</p>}<Button variant="secondary" size="sm" disabled={busy} onClick={retry}>{t("mongo.retry")}</Button></div>;
}

export function MongoManagement({ service }: { service?: ServiceStatus }) {
  const t = useT();
  const [authBusy, setAuthBusy] = React.useState(false);
  const running = !!service?.version && (service.state === "running" || (service.state === "error" && service.pids.length > 0));
  const signature = `${service?.version}:${service?.port}:${service?.pids.join(",")}`;
  return <div className="min-w-0 space-y-5">
    <div className="flex flex-wrap items-start justify-between gap-3"><div className="min-w-0"><h2 className="text-base font-semibold">MongoDB {service?.version ?? ""}</h2><p className="mt-1 text-xs leading-5 text-muted">{t("mongo.intro")}</p></div><Button variant="secondary" size="sm" asChild><Link href="/packages">{t("mongo.packages")}</Link></Button></div>
    {!running ? <Card><CardContent className="py-8"><Database className="mb-3 h-6 w-6 text-muted" /><p className="text-sm">{t(service ? "mongo.stopped" : "mongo.notInstalled")}</p><p className="mt-2 text-xs leading-5 text-muted">{t("mongo.requireShell")}</p></CardContent></Card> : <MongoBrowser key={signature} version={service!.version!} signature={signature} />}
    {!!service?.version && <MongoAuthPanel version={service.version} signature={signature} onLockChange={setAuthBusy} />}
    <MongoBackupPanel service={service} signature={signature} externalDisabled={authBusy} />
  </div>;
}

function MongoBrowser({ version, signature }: { version: string; signature: string }) {
  const t = useT();
  const overview = useQuery({ queryKey: ["mongo-overview", signature], queryFn: () => api.mongoOverview(version), ...options });
  const [database, setDatabase] = React.useState("");
  const [collection, setCollection] = React.useState("");
  const [searchDraft, setSearchDraft] = React.useState("");
  const [search, setSearch] = React.useState("");
  const [offset, setOffset] = React.useState(0);
  const [field, setField] = React.useState("");
  const [valueType, setValueType] = React.useState<MongoFilter["valueType"]>("text");
  const [value, setValue] = React.useState("");
  const [filter, setFilter] = React.useState<MongoFilter | null>(null);
  const databaseExists = !!overview.data?.databases.includes(database);
  const collections = useQuery({ queryKey: ["mongo-collections", signature, database, search], queryFn: () => api.mongoCollections(version, database, search), enabled: databaseExists && !overview.isError && !overview.isFetching, ...options });
  const collectionExists = !!collections.data?.entries.some(entry => entry.name === collection);
  const ready = databaseExists && collectionExists && !overview.isError && !collections.isError && !overview.isFetching && !collections.isFetching;
  const documents = useQuery({ queryKey: ["mongo-documents", signature, database, collection, offset, filter], queryFn: () => api.mongoDocuments(version, database, collection, offset, PAGE_SIZE, filter), enabled: ready, ...options });
  const fieldValid = !!field && new TextEncoder().encode(field).length <= 255 && !/[\x00-\x1f\x7f-\x9f$]/.test(field) && field.split(".").every(Boolean);
  const searchValid = new TextEncoder().encode(searchDraft).length <= 200 && !/[\x00-\x1f\x7f-\x9f]/.test(searchDraft);
  const valueValid = new TextEncoder().encode(value).length <= 4096 && !value.includes("\0") && (valueType === "number" ? /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(value) && Number.isFinite(Number(value)) && (!Number.isInteger(Number(value)) || Number.isSafeInteger(Number(value))) : valueType === "objectId" ? /^[a-f\d]{24}$/i.test(value) : valueType === "boolean" ? ["true", "false"].includes(value) : true);
  const clearFilter = () => { setFilter(null); setField(""); setValue(""); setValueType("text"); setOffset(0); };
  const selectDatabase = (next: string) => { setDatabase(next); setCollection(""); setSearch(""); setSearchDraft(""); clearFilter(); };
  const selectCollection = (next: string) => { setCollection(next); clearFilter(); };
  const loading = overview.isFetching || collections.isFetching || documents.isFetching;
  return <>
    <Card><CardHeader><div className="flex flex-wrap items-center justify-between gap-2"><CardTitle>{t("mongo.connection")}</CardTitle><Button size="sm" variant="ghost" disabled={loading} onClick={() => void overview.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("mongo.refresh")}</Button></div></CardHeader><CardContent>
      {overview.isPending && <p role="status" className="text-sm text-muted">{t("mongo.loading")}</p>}
      {overview.isError ? <><QueryError error={overview.error} busy={overview.isFetching} retry={() => void overview.refetch()} /><p className="mt-3 text-xs text-muted">{t("mongo.requireShell")}</p></> : overview.data && <div className="space-y-3"><div className="flex min-w-0 items-center gap-2"><code className="min-w-0 break-all text-xs">{overview.data.uri}</code><CopyButton text={overview.data.uri} className="shrink-0" /></div><div className="flex flex-wrap gap-x-5 gap-y-2 text-xs text-muted"><span>MongoDB {overview.data.serverVersion}</span><span>MongoDB Shell {overview.data.shellVersion}</span><Badge>{t("mongo.readonly")}</Badge></div>{overview.data.limited && <p className="text-xs text-warn">{t("mongo.databaseLimit")}</p>}</div>}
    </CardContent></Card>
    {!overview.isError && overview.data && <Card><CardHeader><CardTitle>{t("mongo.choose")}</CardTitle></CardHeader><CardContent className="space-y-4">
      {overview.data.databases.length === 0 ? <p className="text-sm text-muted">{t("mongo.noDatabases")}</p> : <div className="grid min-w-0 gap-4 sm:grid-cols-2">
        <div className="min-w-0 space-y-1.5"><Label htmlFor="mongo-database">{t("mongo.database")}</Label><Select value={databaseExists ? database : ""} onValueChange={selectDatabase} disabled={overview.isFetching}><SelectTrigger id="mongo-database" title={database} className="min-w-0"><SelectValue placeholder={t("mongo.selectDatabase")} /></SelectTrigger><SelectContent>{overview.data.databases.map(name => <SelectItem key={name} value={name} className="whitespace-normal break-all">{name}</SelectItem>)}</SelectContent></Select></div>
        <div className="min-w-0 space-y-1.5"><Label htmlFor="mongo-collection">{t("mongo.collection")}</Label><Select value={collectionExists ? collection : ""} onValueChange={selectCollection} disabled={!databaseExists || collections.isFetching || collections.isError || !collections.data?.entries.length}><SelectTrigger id="mongo-collection" title={collection} className="min-w-0"><SelectValue placeholder={t("mongo.selectCollection")} /></SelectTrigger><SelectContent>{collections.data?.entries.map(entry => <SelectItem key={entry.name} value={entry.name} className="whitespace-normal break-all">{entry.name}{entry.kind === "view" ? ` · ${t("mongo.view")}` : entry.kind === "timeseries" ? ` · ${t("mongo.timeseries")}` : ""}</SelectItem>)}</SelectContent></Select></div>
      </div>}
      {databaseExists && <><form className="flex flex-wrap items-end gap-2" onSubmit={event => { event.preventDefault(); if (!searchValid || collections.isFetching || overview.isFetching) return; setCollection(""); setOffset(0); if (search === searchDraft) void collections.refetch(); else setSearch(searchDraft); }}><div className="min-w-0 flex-1 basis-48 space-y-1.5"><Label htmlFor="mongo-collection-search">{t("mongo.searchCollections")}</Label><Input id="mongo-collection-search" value={searchDraft} maxLength={200} aria-invalid={!searchValid} onChange={event => setSearchDraft(event.target.value)} /></div><Button size="sm" variant="secondary" disabled={!searchValid || collections.isFetching || overview.isFetching}><Search className="h-3.5 w-3.5" />{t("mongo.search")}</Button></form>{!searchValid && <p role="alert" className="text-xs text-error">{t("mongo.badSearch")}</p>}
        {collections.isFetching && <p role="status" className="text-xs text-muted">{t("mongo.loading")}</p>}
        {collections.isError && <QueryError error={collections.error} busy={collections.isFetching} retry={() => void collections.refetch()} />}
        {!collections.isFetching && !collections.isError && collections.data?.entries.length === 0 && <p className="text-sm text-muted">{t(search ? "mongo.noCollectionMatch" : "mongo.noCollections")}</p>}
        {collections.data?.limited && <p className="text-xs text-warn">{t("mongo.collectionLimit")}</p>}
      </>}
    </CardContent></Card>}
    {databaseExists && collectionExists && !overview.isError && !collections.isError && <Card className="min-w-0"><CardHeader><div className="flex flex-wrap items-center justify-between gap-2"><CardTitle className="min-w-0 break-all leading-5">{database} / {collection}</CardTitle><Button variant="ghost" size="sm" disabled={loading} onClick={() => void documents.refetch()}><RefreshCw className="h-3.5 w-3.5" />{t("mongo.refresh")}</Button></div><p className="text-xs leading-5 text-muted">{t("mongo.documentsHint")}</p></CardHeader>
      <CardContent className="space-y-4"><form className="space-y-3" onSubmit={event => { event.preventDefault(); if (!ready || documents.isFetching || !fieldValid || !valueValid) return; setOffset(0); const next = {field,value: valueType === "null" ? "" : value,valueType}; if (offset === 0 && JSON.stringify(filter) === JSON.stringify(next)) void documents.refetch(); else setFilter(next); }}>
        <div className="grid min-w-0 gap-3 sm:grid-cols-3"><div className="min-w-0 space-y-1.5"><Label htmlFor="mongo-field">{t("mongo.field")}</Label><Input id="mongo-field" value={field} placeholder="profile.name" maxLength={255} onChange={event => setField(event.target.value)} /></div><div className="min-w-0 space-y-1.5"><Label htmlFor="mongo-type">{t("mongo.type")}</Label><Select value={valueType} onValueChange={next => { setValueType(next as MongoFilter["valueType"]); setValue(next === "boolean" ? "true" : ""); }}><SelectTrigger id="mongo-type"><SelectValue /></SelectTrigger><SelectContent>{(["text", "number", "boolean", "null", "objectId"] as const).map(kind => <SelectItem key={kind} value={kind}>{t(`mongo.type.${kind}`)}</SelectItem>)}</SelectContent></Select></div><div className="min-w-0 space-y-1.5"><Label htmlFor="mongo-value">{t("mongo.value")}</Label>{valueType === "boolean" ? <Select value={value} onValueChange={setValue}><SelectTrigger id="mongo-value"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="true">true</SelectItem><SelectItem value="false">false</SelectItem></SelectContent></Select> : <Input id="mongo-value" value={valueType === "null" ? "null" : value} disabled={valueType === "null"} maxLength={4096} onChange={event => setValue(event.target.value)} aria-invalid={!!value && !valueValid} />}</div></div>
        <div className="flex flex-wrap items-center gap-2"><Button size="sm" variant="secondary" disabled={!ready || documents.isFetching || !fieldValid || !valueValid}>{t("mongo.apply")}</Button><Button type="button" size="sm" variant="ghost" disabled={documents.isFetching || (!field && !filter)} onClick={clearFilter}>{t("mongo.reset")}</Button><span className="text-xs text-muted">{t(filter ? "mongo.filtered" : "mongo.allDocuments")}</span></div>
        {field && !fieldValid && <p role="alert" className="text-xs text-error">{t("mongo.badField")}</p>}{!valueValid && <p role="alert" className="text-xs text-error">{t("mongo.badValue")}</p>}
      </form><div className="border-t border-dashed border-border" />
        {documents.isFetching && <p role="status" className="text-sm text-muted">{t("mongo.loading")}</p>}
        {!documents.isFetching && documents.isError && <QueryError error={documents.error} busy={documents.isFetching} retry={() => void documents.refetch()} />}
        {ready && !documents.isFetching && !documents.isError && documents.data && <><p className="text-xs text-muted">{t("mongo.range").replace("{start}", String(documents.data.documents.length ? offset+1 : 0)).replace("{end}", String(offset+documents.data.documents.length))}</p>
          {!documents.data.documents.length && <p className="py-5 text-sm text-muted">{t(filter ? "mongo.noDocumentsMatch" : "mongo.noDocuments")}</p>}
          <div className="min-w-0 space-y-3">{documents.data.documents.map((document, index) => <details key={`${documents.dataUpdatedAt}:${offset+index}`} open={index === 0} className="min-w-0 rounded-lg bg-fill p-3"><summary className="cursor-pointer text-sm">{t("mongo.document")} {offset+index+1}{document.truncated && <span className="ml-2 text-xs text-warn">{t("mongo.preview")}</span>}</summary><div className="mt-3 min-w-0 space-y-2">{document.truncated ? <p className="text-xs leading-5 text-warn">{t("mongo.truncated")}</p> : <div className="flex justify-end"><CopyButton text={document.content} /></div>}<pre className="max-h-80 overflow-auto whitespace-pre-wrap break-all font-mono text-xs leading-5" tabIndex={0}>{document.content}</pre></div></details>)}</div>
        </>}
        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-dashed border-border pt-4"><Button variant="secondary" size="sm" disabled={!ready || documents.isFetching || offset === 0} onClick={() => setOffset(Math.max(0,offset-PAGE_SIZE))}><ChevronLeft className="h-3.5 w-3.5" />{t("mongo.previous")}</Button><span className="text-xs text-muted">{t("mongo.page").replace("{page}", String(offset/PAGE_SIZE+1))}</span><Button variant="secondary" size="sm" disabled={!ready || documents.isFetching || documents.isError || !documents.data?.hasMore || offset+PAGE_SIZE > 10_000} onClick={() => setOffset(offset+PAGE_SIZE)}>{t("mongo.next")}<ChevronRight className="h-3.5 w-3.5" /></Button></div>
        {offset+PAGE_SIZE > 10_000 && documents.data?.hasMore && <p className="text-xs text-warn">{t("mongo.pageLimit")}</p>}
      </CardContent>
    </Card>}
  </>;
}
