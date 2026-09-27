"use client";

import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw, TerminalSquare } from "lucide-react";
import type { Site, ProjectRuntimeVersions } from "@nsb/schema";
import * as api from "@/lib/api";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectContent, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDialog } from "@/components/shared/misc";
import { CodeBlock } from "@/components/shared/code-block";

/** 固定项目 CLI 版本；启动时由后端再次校验文件与安装状态。 */
export function SiteTerminalButton({ site }: { site: Pick<Site, "id" | "name"> }) {
  const t = useT();
  const qc = useQueryClient();
  const [open, setOpen] = React.useState(false);
  const [tab, setTab] = React.useState("terminal");
  const [busy, setBusy] = React.useState<"open" | "save" | "reload" | null>(null);
  const running = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [saveError, setSaveError] = React.useState<AppErrorShape | null>(null);
  const [opened, setOpened] = React.useState(false);
  const [saved, setSaved] = React.useState(false);
  const [draft, setDraft] = React.useState<{ base: ProjectRuntimeVersions; versions: Record<string, string> } | null>(null);
  const [confirm, setConfirm] = React.useState<"close" | "reload" | null>(null);
  const errorRef = React.useRef<HTMLDivElement>(null);
  const projectErrorRef = React.useRef<HTMLDivElement>(null);
  const environment = useQuery({ queryKey: ["pathenv", "terminal", site.id], queryFn: () => api.terminalEnvironment(site.id), enabled: open && tab === "terminal",
    staleTime: 0, retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false });
  const project = useQuery({ queryKey: ["project-runtimes", site.id], queryFn: () => api.projectRuntimeVersions(site.id), enabled: open && tab === "project",
    staleTime: 0, retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false });
  React.useEffect(() => {
    if (project.data && !draft && open && tab === "project" && !project.isFetching && !project.error) setDraft({ base: project.data, versions: { ...project.data.versions } });
  }, [project.data, project.isFetching, project.error, draft, open, tab]);
  const dirty = !!draft && (Object.keys(draft.versions).length !== Object.keys(draft.base.versions).length
    || Object.entries(draft.versions).some(([id, version]) => draft.base.versions[id] !== version));
  const invalid = !!draft && (Object.entries(draft.versions).some(([id, version]) => !draft.base.options.find((o) => o.id === id)?.versions.includes(version))
    || draft.base.detected.some((entry) => entry.issue && !Object.hasOwn(draft.versions, entry.id)));
  const problem = error ?? (environment.error ? normalizeError(environment.error) : null);
  const projectProblem = saveError ?? (project.error ? normalizeError(project.error) : null);
  React.useEffect(() => { if (problem && tab === "terminal") errorRef.current?.focus(); }, [problem?.message, tab]);
  React.useEffect(() => { if (projectProblem && tab === "project") projectErrorRef.current?.focus(); }, [projectProblem?.message, tab]);
  React.useEffect(() => {
    if (!open || !dirty) return;
    const guard = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", guard);
    return () => window.removeEventListener("beforeunload", guard);
  }, [open, dirty]);
  const refresh = async () => {
    if (running.current || environment.isFetching) return;
    setError(null); setOpened(false);
    await environment.refetch({ cancelRefetch: false });
  };
  const reloadProject = async () => {
    if (running.current || project.isFetching) return;
    running.current = true; setBusy("reload"); setSaved(false); setSaveError(null);
    try {
      const result = await project.refetch({ cancelRefetch: false });
      if (result.data && !result.error) setDraft({ base: result.data, versions: { ...result.data.versions } });
    } finally { running.current = false; setBusy(null); }
  };
  const save = async () => {
    if (running.current || project.isFetching || !draft || !dirty || invalid) return;
    running.current = true; setBusy("save"); setSaveError(null); setSaved(false);
    try {
      const view = await api.saveProjectRuntimeVersions(site.id, draft.versions, draft.base.revision);
      setDraft({ base: view, versions: { ...view.versions } }); qc.setQueryData(["project-runtimes", site.id], view);
      setSaved(true); setError(null); setOpened(false);
      await qc.invalidateQueries({ queryKey: ["pathenv", "terminal"] });
      await qc.invalidateQueries({ queryKey: ["project-runtimes"], predicate: (query) => query.queryKey[1] !== site.id });
    } catch (e) { setSaveError(normalizeError(e)); }
    finally { running.current = false; setBusy(null); }
  };
  const launch = async () => {
    if (running.current || dirty || environment.isFetching || environment.error || !environment.data || !isTauri || error?.code === "TERMINAL_ENV_CHANGED") return;
    running.current = true; setBusy("open"); setError(null); setOpened(false);
    try { await api.openTerminal(environment.data.revision, site.id); setOpened(true); }
    catch (e) { setError(normalizeError(e)); }
    finally { running.current = false; setBusy(null); }
  };
  const close = () => { setOpen(false); setDraft(null); setConfirm(null); };
  const data = environment.data;
  return <>
    <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" aria-label={t("dashboard.openTerminal")}
      onClick={() => { setError(null); setSaveError(null); setOpened(false); setSaved(false); setDraft(null); setTab("terminal"); setOpen(true); }}><TerminalSquare className="h-3.5 w-3.5" /></Button></TooltipTrigger><TooltipContent>{t("sites.terminal.title")}</TooltipContent></Tooltip>
    <Dialog open={open} onOpenChange={(next) => { if (running.current) return; if (!next && dirty) setConfirm("close"); else if (!next) close(); else setOpen(true); }}>
      <DialogContent hideClose={!!busy} className="flex max-h-[85dvh] max-w-xl flex-col overflow-hidden">
        <DialogHeader className="shrink-0 pr-5"><DialogTitle title={site.name} className="line-clamp-2 leading-snug [overflow-wrap:anywhere]">{t("sites.terminal.title")} · {site.name}</DialogTitle><DialogDescription>{t("sites.terminal.hint")}</DialogDescription></DialogHeader>
        <Tabs value={tab} onValueChange={setTab} className="flex min-h-0 flex-1 flex-col gap-3">
          <TabsList className="shrink-0 self-start"><TabsTrigger value="terminal" disabled={!!busy}>{t("sites.terminal.preview")}</TabsTrigger><TabsTrigger value="project" disabled={!!busy}>{t("sites.project.title")}{dirty && <span aria-label={t("detail.unsaved")}> •</span>}</TabsTrigger></TabsList>
          <TabsContent value="terminal" className="mt-0 min-h-0 min-w-0 space-y-4 overflow-y-auto text-xs leading-relaxed" aria-busy={environment.isFetching}>
          {!isTauri && <p className="text-warn">{t("sites.terminal.demo")}</p>}
          {dirty && <p role="status" className="rounded-lg bg-warn-soft p-3 text-warn">{t("sites.project.pending")}</p>}
          {environment.isFetching && <p role="status" className="text-secondary">{t("common.loading")}</p>}
          {problem && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-2 rounded-lg bg-error-soft p-3 text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]"><p>{problem.message}</p>{problem.hint && <p>{problem.hint}</p>}</div>}
          {data && !environment.error && !environment.isFetching && <>
            <div className="space-y-1"><p className="text-secondary">{t("sites.terminal.directory")}</p><p className="font-mono [overflow-wrap:anywhere]">{data.cwd}</p></div>
            {data.warnings.length > 0 && <div role="status" className="rounded-lg bg-warn-soft p-3 text-warn"><p>{t("tools.termInjectSkipped")}</p><ul className="mt-2 list-disc space-y-1 pl-4 [overflow-wrap:anywhere]">{data.warnings.map((warning, i) => <li key={i}>{warning}</li>)}</ul></div>}
            <div className="rounded-lg bg-fill px-3">{data.entries.map((entry) => <div key={entry.id} className="space-y-1 border-b border-dashed border-separator py-3 last:border-0"><p className="flex flex-wrap items-center justify-between gap-2"><span className="min-w-0 font-medium [overflow-wrap:anywhere]">{entry.label}</span><code className="min-w-0 [overflow-wrap:anywhere]">{entry.version}</code></p>{entry.source && <p className="text-secondary [overflow-wrap:anywhere]">{t("sites.project.source").replace("{source}", entry.source)}</p>}<p className="font-mono text-secondary [overflow-wrap:anywhere]">{entry.binDir}</p></div>)}
              {data.entries.length === 0 && <p className="py-3 text-secondary">{t("sites.terminal.empty")}</p>}</div>
            <p className="text-secondary">{t("sites.terminal.selectionHint")}</p>
            <p className="text-secondary">{t("sites.terminal.scope")}</p>
            {data.script && <details><summary className="cursor-pointer text-secondary">{t("sites.terminal.script")}</summary><div className="mt-3"><CodeBlock code={data.script} lang="shell" title={data.shell === "powershell" ? "PowerShell" : "Bash / Zsh"} maxHeight={200} compact /></div></details>}
          </>}
          {opened && <p role="status" className="rounded-lg bg-fill p-3 text-secondary">{t("sites.terminal.opened")}</p>}
          </TabsContent>
          <TabsContent value="project" className="mt-0 min-h-0 min-w-0 space-y-4 overflow-y-auto text-xs leading-relaxed" aria-busy={!!busy || project.isFetching}>
            <p className="text-secondary">{t("sites.project.hint")}</p>
            {!isTauri && <p className="text-warn">{t("sites.project.demo")}</p>}
            {project.isFetching && <p role="status" className="text-secondary">{t("common.loading")}</p>}
            {projectProblem && <div ref={projectErrorRef} tabIndex={-1} role="alert" className="space-y-2 rounded-lg bg-error-soft p-3 text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]"><p>{projectProblem.message}</p>{projectProblem.hint && <p>{projectProblem.hint}</p>}</div>}
            {draft && <>
              {draft.base.sharedSites.length > 0 && <p className="rounded-lg bg-warn-soft p-3 text-warn [overflow-wrap:anywhere]">{t("sites.project.shared").replace("{sites}", draft.base.sharedSites.join("、"))}</p>}
              <div className="rounded-lg bg-fill px-3">{draft.base.options.map((option) => {
                const selected = Object.hasOwn(draft.versions, option.id) ? draft.versions[option.id] : undefined;
                const detected = draft.base.detected.find((entry) => entry.id === option.id);
                const missing = !!selected && !option.versions.includes(selected);
                const inherit = detected ? (detected.resolvedVersion ? t("sites.project.autoVersion").replace("{version}", detected.resolvedVersion) : t("sites.project.autoIssue")) : option.id === "php" && draft.base.phpVersion
                  ? t("sites.project.followPhp").replace("{version}", draft.base.phpVersion) : t("sites.project.followPath");
                return <div key={option.id} className="space-y-2 border-b border-dashed border-separator py-3 last:border-0">
                  <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
                    <span className="min-w-0 font-medium [overflow-wrap:anywhere]">{option.label}</span>
                    <Select value={selected ?? "__inherit"} disabled={!!busy || project.isFetching} onValueChange={(value) => {
                      setDraft((current) => { if (!current) return current; const versions = { ...current.versions }; if (value === "__inherit") delete versions[option.id]; else versions[option.id] = value; return { ...current, versions }; });
                      setSaved(false); setSaveError(null);
                    }}>
                      <SelectTrigger aria-label={option.label} aria-invalid={missing || (!!detected?.issue && !selected) || undefined} className="min-w-0 sm:w-64 sm:shrink-0"><SelectValue /></SelectTrigger>
                      <SelectContent><SelectItem value="__inherit">{inherit}</SelectItem>{(option.versions.length > 0 || missing) && <SelectSeparator />}
                        {missing && <SelectItem value={selected} disabled>{selected} · {t("sites.project.unavailable")}</SelectItem>}
                        {option.versions.map((version) => <SelectItem key={version} value={version}>{version}</SelectItem>)}
                      </SelectContent>
                    </Select>
                  </div>
                  {detected && <div className="space-y-1 text-secondary [overflow-wrap:anywhere]">
                    <p>{t("sites.project.source").replace("{source}", detected.requirements.length ? detected.requirements.join(" · ") : detected.files.join(" · "))}</p>
                    {selected ? <p>{t(selected === draft.base.versions[option.id] ? "sites.project.overridden" : "sites.project.overrideDraft")}</p> : detected.issue ? <p role="status" className="text-warn">{detected.issue}</p> : <p>{t("sites.project.autoHint")}</p>}
                  </div>}
                  {missing && <p className="text-warn">{t("sites.project.missing")}</p>}
                </div>;
              })}{draft.base.options.length === 0 && <p className="py-3 text-secondary">{t("sites.project.empty")}</p>}</div>
              <p className="text-secondary">{t("sites.project.phpScope")}</p>
              <details><summary className="cursor-pointer text-secondary">{t("sites.project.file")}</summary><div className="mt-2 space-y-2"><p className="font-mono [overflow-wrap:anywhere]">{draft.base.path}</p><p className="text-secondary">{t("sites.project.backup")}</p><p className="text-secondary">{t("sites.project.detectScope")}</p></div></details>
              {dirty && <p role="status" className="text-warn">{t("detail.unsaved")}</p>}
            </>}
            {saved && <p role="status" className="rounded-lg bg-fill p-3 text-secondary">{t(isTauri ? "sites.project.saved" : "sites.project.demoSaved")}</p>}
          </TabsContent>
        </Tabs>
        {tab === "project" ? <DialogFooter className="shrink-0 flex-wrap gap-2"><Button variant="ghost" disabled={!!busy || project.isFetching} onClick={() => dirty ? setConfirm("reload") : void reloadProject()}><RefreshCw className="h-3.5 w-3.5" />{t("env.reload")}</Button><Button disabled={!!busy || project.isFetching || !draft || !dirty || invalid} onClick={() => void save()}>{t(busy === "save" ? "sites.project.saving" : "sites.project.save")}</Button></DialogFooter> : <DialogFooter className="shrink-0 flex-wrap gap-2"><Button variant="ghost" disabled={!!busy || environment.isFetching} onClick={() => void refresh()}><RefreshCw className="h-3.5 w-3.5" />{t("tools.refresh")}</Button><Button disabled={!!busy || dirty || environment.isFetching || !!environment.error || error?.code === "TERMINAL_ENV_CHANGED" || !data || !isTauri} onClick={() => void launch()}>{t(busy === "open" ? "sites.terminal.opening" : "dashboard.openTerminal")}</Button></DialogFooter>}
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={confirm !== null} onOpenChange={(next) => { if (!next) setConfirm(null); }} title={t("cfgeditor.discardTitle")} description={t("sites.project.discard")}
      confirmText={t(confirm === "reload" ? "env.reload" : "cfgeditor.discard")} onConfirm={() => { const action = confirm; setConfirm(null); if (action === "close") close(); else void reloadProject(); }} />
  </>;
}
