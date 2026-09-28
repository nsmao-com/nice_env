"use client";

import * as React from "react";
import { ProjectPhpCheck, projectPhpProblem, recommendedProjectPhp } from "./project-php-compatibility";
import { useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { FolderSearch, Loader2, BadgeCheck } from "lucide-react";
import { SiteKind, RewritePreset, type ScannedProject, type CreateSiteInput, type Site } from "@nsb/schema";
import { useT, useUI } from "@/lib/store";
import { useInvalidate, usePackages, useSites, toastError } from "@/lib/hooks";
import { isTauri, normalizeError } from "@/lib/backend";
import * as api from "@/lib/api";
import { cn, cmpVersionDesc, normalizeProxyTarget } from "@/lib/utils";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

type ProjectDraft = { domain: string; phpVersion: string; proxyTarget: string; selected: boolean; allowUnverifiedPhp?: boolean };
type Outcome = { status: "creating" | "created" | "error"; message?: string; siteId?: string };
const PAGE_SIZE = 5;

/** 扫描建议只填入未占用的本地域名；用户后续编辑不会被轮询覆盖。 */
export function availableScanDomain(suggested: string, occupied: Set<string>): string {
  const [label, ...suffix] = suggested.toLowerCase().split(".");
  let domain = suggested.toLowerCase();
  let number = 1;
  while (occupied.has(domain)) {
    const ending = `-${++number}`;
    domain = `${label.slice(0, 63 - ending.length)}${ending}.${suffix.join(".")}`;
  }
  occupied.add(domain);
  return domain;
}

export function scannedProjectProblem(project: ScannedProject, draft: ProjectDraft, phpVersions: string[], occupied: Set<string>, duplicate: boolean) {
  if (!SiteKind.safeParse(project.siteKind).success) return "scanSetup.unsupported" as const;
  if (!project.needsDevServer && !project.documentRootReady) return "siteResume.missing" as const;
  const domain = draft.domain.trim().toLowerCase();
  if (domain.length > 253 || !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+$/i.test(domain)) return "scanSetup.invalidDomain" as const;
  if (occupied.has(domain) || duplicate) return "scanSetup.domainUsed" as const;
  if (project.siteKind === "php" && !phpVersions.includes(draft.phpVersion)) return "scanSetup.phpRequired" as const;
  if (project.siteKind === "php") {
    const problem = projectPhpProblem(project.phpCompatibility, draft.phpVersion, !!draft.allowUnverifiedPhp);
    if (problem) return problem;
  }
  if (project.needsDevServer && !normalizeProxyTarget(draft.proxyTarget)) return "sites.proxy.invalidTarget" as const;
  return null;
}

export function scannedSiteInput(project: ScannedProject, draft: ProjectDraft, webServer: "nginx" | "apache", https: boolean): CreateSiteInput {
  return {
    name: project.name,
    domains: [draft.domain.trim().toLowerCase()],
    rootDir: project.needsDevServer ? project.path : project.documentRoot,
    runtime: {
      webServer,
      kind: SiteKind.parse(project.siteKind),
      ...(project.siteKind === "php" ? { phpVersion: draft.phpVersion } : {}),
      ...(project.needsDevServer ? { proxyTarget: normalizeProxyTarget(draft.proxyTarget)! } : {}),
    },
    https,
    rewrite: project.needsDevServer ? "none" : RewritePreset.safeParse(project.rewrite).data ?? "none",
    template: "none",
    writeEnvExample: false,
  };
}

/** 先确认每个项目的入口和运行环境，再顺序建站；失败项保留配置供修正重试。 */
export function ProjectScannerDialog({ open, onOpenChange, onCreated }: {
  open: boolean; onOpenChange: (value: boolean) => void; onCreated?: () => void;
}) {
  const t = useT();
  const router = useRouter();
  const client = useQueryClient();
  const invalidate = useInvalidate();
  const packageQuery = usePackages();
  const siteQuery = useSites();
  const phpVersions = [...new Set(packageQuery.data.filter((p) => p.id === "php" && p.install).map((p) => p.version))].sort(cmpVersionDesc);
  const webServers = (["nginx", "apache"] as const).filter((id) => packageQuery.data.some((p) => p.id === id && p.install));
  const defaultWeb = webServers[0];
  const occupied = new Set(siteQuery.data.flatMap((site) => site.domains.map((domain) => domain.toLowerCase())));
  const [root, setRoot] = React.useState("");
  const [found, setFound] = React.useState<ScannedProject[] | null>(null);
  const [drafts, setDrafts] = React.useState<Record<string, ProjectDraft>>({});
  const [outcomes, setOutcomes] = React.useState<Record<string, Outcome>>({});
  const [mode, setMode] = React.useState<"scanning" | "picking" | "creating" | "checking" | null>(null);
  const busyRef = React.useRef(false);
  const checkingPath = React.useRef<string | null>(null);
  const mounted = React.useRef(true);
  const handingOff = React.useRef(false);
  const [error, setError] = React.useState("");
  const [query, setQuery] = React.useState("");
  const [page, setPage] = React.useState(1);
  const [webServer, setWebServer] = React.useState<"nginx" | "apache" | "">("");
  const [bulkPhp, setBulkPhp] = React.useState("");
  const [https, setHttps] = React.useState(false);
  const [attempted, setAttempted] = React.useState(false);
  const [focusIndex, setFocusIndex] = React.useState<number | null>(null);
  const [progress, setProgress] = React.useState<{ done: number; total: number; name: string } | null>(null);
  const [report, setReport] = React.useState<{ ok: number; fail: number } | null>(null);
  const errorRef = React.useRef<HTMLParagraphElement>(null);
  const panelRef = React.useRef<HTMLDivElement>(null);
  const busy = mode !== null;
  const queryError = packageQuery.error || siteQuery.error;

  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  React.useEffect(() => { if (open) handingOff.current = false; }, [open]);
  React.useEffect(() => { if (open && !webServer && defaultWeb) setWebServer(defaultWeb); }, [open, webServer, defaultWeb]);
  React.useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);
  React.useEffect(() => {
    if (focusIndex !== null) {
      const row = panelRef.current?.querySelector<HTMLElement>(`[data-project-index="${focusIndex}"]`);
      (row?.querySelector<HTMLElement>('[aria-invalid="true"]:not(:disabled)') ?? row?.querySelector<HTMLElement>('[role="alert"]'))?.focus();
      setFocusIndex(null);
    }
  }, [focusIndex, page, query]);

  const selected = (found ?? []).filter((p) => drafts[p.path]?.selected && outcomes[p.path]?.status !== "created");
  const missingPhp = selected.some((p) => p.siteKind === "php") && !phpVersions.length;
  const filtered = (found ?? []).filter((p) => `${p.name} ${p.path} ${p.kind}`.toLowerCase().includes(query.trim().toLowerCase()));
  const pages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const currentPage = Math.min(page, pages);
  const selectable = (p: ScannedProject) => (p.documentRootReady || p.needsDevServer) && outcomes[p.path]?.status !== "created";
  const choices = filtered.filter(selectable);
  const allSelected = choices.length > 0 && choices.every((p) => drafts[p.path]?.selected);
  const problem = (p: ScannedProject) => scannedProjectProblem(p, drafts[p.path], phpVersions, occupied,
    selected.some((other) => other.path !== p.path && drafts[other.path].domain.trim().toLowerCase() === drafts[p.path].domain.trim().toLowerCase()));
  const patch = (path: string, value: Partial<ProjectDraft>) => {
    if (!busyRef.current) setDrafts((previous) => ({ ...previous, [path]: { ...previous[path], ...value } }));
  };
  const reset = () => { setFound(null); setDrafts({}); setOutcomes({}); setReport(null); setAttempted(false); setError(""); setQuery(""); setPage(1); };
  const setWorking = (next: typeof mode) => { busyRef.current = next !== null; if (mounted.current) setMode(next); };

  // 调用方持 busyRef，禁止输入路径、文件选择与重复扫描在结果返回前交错。
  const scanFrom = async (dir: string) => {
    setWorking("scanning"); reset();
    const list = await api.scanProjects(dir);
    if (!mounted.current) return;
    const reserved = new Set(occupied);
    const next: Record<string, ProjectDraft> = {};
    for (const p of list) next[p.path] = {
      domain: availableScanDomain(p.suggestedDomain, reserved), phpVersion: recommendedProjectPhp(p.phpCompatibility, phpVersions, bulkPhp), proxyTarget: "", allowUnverifiedPhp: false,
      selected: !p.alreadyConfigured && p.documentRootReady && !p.needsDevServer,
    };
    setFound(list); setDrafts(next);
  };
  const startScan = async (pick: boolean) => {
    if (busyRef.current || (!pick && !root.trim())) return;
    setWorking(pick ? "picking" : "scanning"); setError("");
    try {
      let dir = root.trim();
      if (pick) {
        const { open: choose } = await import("@tauri-apps/plugin-dialog");
        const result = await choose({ directory: true, multiple: false, title: t("scanner.pickTitle") });
        if (typeof result !== "string" || !mounted.current) return;
        dir = result; setRoot(dir);
      }
      await scanFrom(dir);
    } catch (failure) {
      if (mounted.current) { const detail = normalizeError(failure); setError([detail.message, detail.hint].filter(Boolean).join(" · ")); }
    } finally { setWorking(null); }
  };

  const refreshPhp = async (project: ScannedProject) => {
    if (busyRef.current) return;
    checkingPath.current = project.path; setWorking("checking"); setError("");
    setFound((old) => old?.map((item) => item.path === project.path ? { ...item, phpCompatibility: null } : item) ?? null);
    setDrafts((old) => ({ ...old, [project.path]: { ...old[project.path], allowUnverifiedPhp: false } }));
    try {
      const report = await api.projectPhpCompatibility(project.path);
      if (!mounted.current) return;
      setFound((old) => old?.map((item) => item.path === project.path ? { ...item, phpCompatibility: report } : item) ?? null);
      setDrafts((old) => ({ ...old, [project.path]: { ...old[project.path], phpVersion: old[project.path].phpVersion || recommendedProjectPhp(report, phpVersions) } }));
    } catch (failure) {
      if (mounted.current) {
        const message = normalizeError(failure).message; setError(message);
        setFound((old) => old?.map((item) => item.path === project.path ? { ...item, phpCompatibility: { status: "unavailable", requirement: null, versions: [], matchingVersions: [], message } as const } : item) ?? null);
      }
    }
    finally { checkingPath.current = null; setWorking(null); }
  };

  const createAll = async () => {
    if (busyRef.current || !selected.length || !webServer || !webServers.includes(webServer) || queryError) return;
    setAttempted(true);
    const invalid = selected.find((p) => problem(p));
    if (invalid) {
      const index = found!.indexOf(invalid); setQuery(""); setPage(Math.floor(index / PAGE_SIZE) + 1); setFocusIndex(index);
      return;
    }
    const targets = selected.map((project) => ({ project, input: scannedSiteInput(project, drafts[project.path], webServer, https), allowUnverifiedPhp: !!drafts[project.path].allowUnverifiedPhp }));
    setWorking("creating"); setError(""); setReport(null);
    let ok = 0; let fail = 0;
    try {
      for (const [index, { project, input, allowUnverifiedPhp }] of targets.entries()) {
        if (!mounted.current) break;
        setProgress({ done: index, total: targets.length, name: project.name });
        setOutcomes((old) => ({ ...old, [project.path]: { status: "creating" } }));
        try {
          const site = await api.createSite(input, project.path, allowUnverifiedPhp);
          ok += 1;
          client.setQueryData<Site[]>(["sites"], (old) => [...(old ?? []).filter((item) => item.id !== site.id), site]);
          if (mounted.current) {
            setOutcomes((old) => ({ ...old, [project.path]: { status: "created", siteId: site.id } }));
            setDrafts((old) => ({ ...old, [project.path]: { ...old[project.path], selected: false } }));
          }
        } catch (failure) {
          fail += 1;
          const detail = normalizeError(failure);
          if (mounted.current) setOutcomes((old) => ({ ...old, [project.path]: { status: "error", message: [detail.message, detail.hint].filter(Boolean).join(" · ") } }));
        }
      }
      if (mounted.current) {
        setReport({ ok, fail });
        const message = fail ? t("scanner.createdPartial").replace("{ok}", String(ok)).replace("{fail}", String(fail)) : t("scanner.createdN").replace("{n}", String(ok));
        (fail ? toast.warning : toast.success)(message);
      }
      if (ok && mounted.current) onCreated?.();
    } finally { invalidate("sites", "hosts", "certs", "services"); if (mounted.current) setProgress(null); setWorking(null); }
  };

  const configure = (project: ScannedProject) => {
    if (busyRef.current || !selectable(project)) return;
    handingOff.current = true; onOpenChange(false);
    const draft = drafts[project.path];
    useUI.getState().openExistingProject(project, {
      domain: draft.domain, phpVersion: draft.phpVersion, proxyTarget: draft.proxyTarget,
      webServer: webServer || undefined, https,
    });
  };

  return <Dialog open={open} onOpenChange={(value) => { if (!busyRef.current) onOpenChange(value); }}>
    <DialogContent ref={panelRef} hideClose={busy} className="flex max-h-[90dvh] max-w-4xl flex-col gap-0 overflow-hidden p-0"
      onCloseAutoFocus={(event) => { if (handingOff.current) event.preventDefault(); }}>
      <DialogHeader className="mx-4 shrink-0 border-b border-dashed border-separator py-4 sm:mx-5">
        <DialogTitle className="flex items-center gap-2 pr-10 text-base"><FolderSearch className="size-4 shrink-0 text-primary" />{t("scanner.title")}</DialogTitle>
        <DialogDescription className="pr-6 text-xs leading-relaxed">{t("scanner.subtitle")}</DialogDescription>
      </DialogHeader>
      <div className="min-h-0 min-w-0 flex-1 space-y-4 overflow-y-auto px-4 py-4 sm:px-5">
        <div className="space-y-2">
          <Label htmlFor="scan-root">{t("scanSetup.root")}</Label>
          <div className="flex flex-wrap gap-2">
            <Input id="scan-root" value={root} disabled={busy} className="min-w-0 flex-1 basis-48 font-mono text-xs"
              onChange={(event) => { if (!busyRef.current) { setRoot(event.target.value); reset(); } }} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); void startScan(false); } }} placeholder={t("scanner.pathPlaceholder")} />
            <Button variant="secondary" disabled={busy || !isTauri} onClick={() => void startScan(true)}>{t("scanner.browse")}</Button>
            <Button disabled={busy || !root.trim()} onClick={() => void startScan(false)}>{mode === "scanning" && <Loader2 className="size-4 animate-spin motion-reduce:animate-none" />}{t("scanner.scan")}</Button>
          </div>
          {!isTauri && <p className="text-xs leading-relaxed text-muted">{t("scanSetup.preview")}</p>}
          {error && <p ref={errorRef} tabIndex={-1} role="alert" className="rounded-lg bg-error-soft p-3 text-xs leading-relaxed text-error [overflow-wrap:anywhere]">{error}</p>}
        </div>
        {mode === "scanning" && <p role="status" className="text-xs text-muted">{t("common.loading")}</p>}
        {found && found.length > 0 && <>
          <section className="space-y-3 rounded-xl bg-fill p-3 sm:p-4">
            <p className="text-xs font-medium">{t("scanSetup.settings")}</p>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="space-y-1.5"><Label htmlFor="scan-web">{t("sites.wizard.webServer")}</Label>
                <Select value={webServer} disabled={busy || !webServers.length} onValueChange={(value) => setWebServer(value as "nginx" | "apache")}>
                  <SelectTrigger id="scan-web"><SelectValue placeholder={t("scanSetup.webRequired")} /></SelectTrigger><SelectContent>{webServers.map((id) => <SelectItem key={id} value={id}>{id === "nginx" ? "Nginx" : "Apache"}</SelectItem>)}</SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5"><Label htmlFor="scan-php-all">{t("scanSetup.bulkPhp")}</Label>
                <Select value={bulkPhp} disabled={busy || !phpVersions.length} onValueChange={(value) => {
                  setBulkPhp(value); setDrafts((old) => Object.fromEntries(Object.entries(old).map(([path, draft]) => [path,
                    draft.selected && found.some((p) => p.path === path && p.siteKind === "php") ? { ...draft, phpVersion: value, allowUnverifiedPhp: false } : draft])));
                }}><SelectTrigger id="scan-php-all"><SelectValue placeholder={t("scanSetup.phpChoose")} /></SelectTrigger><SelectContent>{phpVersions.map((version) => <SelectItem key={version} value={version}>PHP {version}</SelectItem>)}</SelectContent></Select>
              </div>
            </div>
            <p className="text-xs leading-relaxed text-muted">{t("scanSetup.phpHint")}</p>
            <label className="flex items-center justify-between gap-3 text-xs"><span>{t("sites.wizard.https")}</span><Switch checked={https} disabled={busy} onCheckedChange={setHttps} /></label>
            <p className="text-xs leading-relaxed text-muted">{t(https ? "scanSetup.httpsHint" : "scanSetup.preserve")}</p>
            {(queryError || missingPhp || !webServers.length || !webServers.includes(webServer as "nginx" | "apache")) && <div role="alert" className="space-y-2 text-xs text-error">
              <p className="[overflow-wrap:anywhere]">{queryError ? normalizeError(queryError).message : t(missingPhp && webServers.length ? "scanSetup.phpRequired" : "scanSetup.webRequired")}</p>
              <div className="flex flex-wrap gap-2"><Button size="sm" variant="secondary" disabled={busy} onClick={() => { void packageQuery.refetch(); void siteQuery.refetch(); }}>{t("siteFiles.retry")}</Button>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => { if (!busyRef.current) { onOpenChange(false); router.push("/packages"); } }}>{t("scanSetup.packages")}</Button></div>
            </div>}
          </section>
          <div className="flex flex-wrap items-center gap-2">
            <Input aria-label={t("scanSetup.search")} placeholder={t("scanSetup.search")} value={query} onChange={(event) => { setQuery(event.target.value); setPage(1); }} className="min-w-0 flex-1 basis-48" />
            <Button variant="secondary" size="sm" disabled={busy || !choices.length} onClick={() => {
              if (busyRef.current) return;
              const paths = new Set(choices.map((p) => p.path));
              setDrafts((old) => Object.fromEntries(Object.entries(old).map(([path, draft]) => [path, paths.has(path) ? { ...draft, selected: !allSelected, phpVersion: draft.phpVersion || bulkPhp } : draft])));
            }}>{t(allSelected ? "scanSetup.clearFiltered" : "scanSetup.selectFiltered")}</Button>
          </div>
          {!filtered.length && <p role="status" className="rounded-lg bg-fill p-4 text-xs text-muted">{t("scanSetup.noResults")}</p>}
          <div className="space-y-3">{filtered.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE).map((project) => {
            const draft = drafts[project.path]; const outcome = outcomes[project.path]; const index = found.indexOf(project);
            const issue = attempted && draft.selected && outcome?.status !== "created" ? problem(project) : null;
            const invalidDomain = issue === "scanSetup.invalidDomain" || issue === "scanSetup.domainUsed";
            const disabled = busy || outcome?.status === "created";
            return <article key={project.path} data-project-index={index} aria-busy={outcome?.status === "creating"} className={cn("min-w-0 space-y-3 rounded-xl border p-3 sm:p-4", draft.selected ? "border-primary/40 bg-primary-soft" : "border-border")}>
              <div className="flex flex-wrap items-start justify-between gap-2">
                <label className="flex min-w-0 flex-1 items-start gap-2.5">
                  <input type="checkbox" className="mt-0.5 size-4 shrink-0 accent-primary" checked={draft.selected} disabled={disabled || !selectable(project)} onChange={(event) => patch(project.path, { selected: event.target.checked, phpVersion: draft.phpVersion || bulkPhp })} />
                  <span className="min-w-0 text-sm font-medium [overflow-wrap:anywhere]">{project.name}</span>
                </label>
                <Badge variant="outline">{project.kind}</Badge>
              </div>
              <p className="font-mono text-xs text-muted [overflow-wrap:anywhere]">{project.needsDevServer ? project.path : project.documentRoot}</p>
              {project.alreadyConfigured && <p className="text-xs text-muted">{t("siteResume.already")}</p>}
              {!project.documentRootReady && !project.needsDevServer && <p className="text-xs leading-relaxed text-error">{t("siteResume.missing")}</p>}
              {selectable(project) && <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1.5"><Label htmlFor={`scan-domain-${index}`}>{t("sites.wizard.domain")}</Label>
                  <Input id={`scan-domain-${index}`} value={draft.domain} disabled={disabled} spellCheck={false} autoCapitalize="none" aria-invalid={invalidDomain} aria-describedby={issue ? `scan-error-${index}` : undefined}
                    onChange={(event) => patch(project.path, { domain: event.target.value })} className="font-mono text-xs" />
                </div>
                {project.siteKind === "php" && <div className="space-y-1.5"><Label htmlFor={`scan-php-${index}`}>{t("sites.wizard.phpVersion")}</Label>
                  <Select value={draft.phpVersion} disabled={disabled || !phpVersions.length} onValueChange={(value) => patch(project.path, { phpVersion: value, allowUnverifiedPhp: false })}>
                    <SelectTrigger id={`scan-php-${index}`} aria-invalid={issue === "scanSetup.phpRequired" || issue?.startsWith("projectPhp.")} aria-describedby={issue ? `scan-error-${index}` : undefined}><SelectValue placeholder={t("scanSetup.phpChoose")} /></SelectTrigger><SelectContent>{phpVersions.map((version) => <SelectItem key={version} value={version}>PHP {version}</SelectItem>)}</SelectContent>
                  </Select>
                </div>}
                {project.needsDevServer && <div className="space-y-1.5"><Label htmlFor={`scan-proxy-${index}`}>{t("sites.wizard.proxyTarget")}</Label>
                  <Input id={`scan-proxy-${index}`} value={draft.proxyTarget} disabled={disabled} placeholder="127.0.0.1:3001" spellCheck={false} autoCapitalize="none" aria-invalid={issue === "sites.proxy.invalidTarget"} aria-describedby={`scan-proxy-hint-${index}${issue ? ` scan-error-${index}` : ""}`}
                    onChange={(event) => patch(project.path, { proxyTarget: event.target.value })} className="font-mono text-xs" />
                </div>}
              </div>}
              {outcome?.status === "created" && <div className="flex flex-wrap items-center gap-2">
                <p className="min-w-0 font-mono text-xs [overflow-wrap:anywhere]">{draft.domain}</p>
                <Button size="sm" variant="ghost" disabled={busy || !isTauri} onClick={() => { if (outcome.siteId) void api.openSite(outcome.siteId).catch(toastError); }}>{t("scanSetup.open")}</Button>
              </div>}
              {project.siteKind === "php" && <ProjectPhpCheck report={project.phpCompatibility} version={draft.phpVersion} acknowledged={!!draft.allowUnverifiedPhp} disabled={disabled} loading={mode === "checking" && checkingPath.current === project.path}
                onAcknowledge={(value) => patch(project.path, { allowUnverifiedPhp: value })} onRefresh={() => void refreshPhp(project)} />}
              {project.phpMinVersion && <p className="text-xs leading-relaxed text-muted">{t("siteResume.php").replace("{version}", project.phpMinVersion)}</p>}
              {project.needsDevServer && <p id={`scan-proxy-hint-${index}`} className="text-xs leading-relaxed text-muted">{t("scanSetup.proxyHint")}{normalizeProxyTarget(draft.proxyTarget) && <span className="block font-mono [overflow-wrap:anywhere]">{normalizeProxyTarget(draft.proxyTarget)}</span>}</p>}
              {issue && <p id={`scan-error-${index}`} tabIndex={-1} role="alert" className="text-xs leading-relaxed text-error">{t(issue)}</p>}
              {outcome && <p role={outcome.status === "error" ? "alert" : "status"} className={cn("flex items-start gap-2 text-xs leading-relaxed [overflow-wrap:anywhere]", outcome.status === "error" ? "text-error" : "text-running")}>
                {outcome.status === "created" && <BadgeCheck className="size-4 shrink-0" />}{outcome.status === "creating" && <Loader2 className="size-4 shrink-0 animate-spin motion-reduce:animate-none" />}
                {outcome.status === "error" ? outcome.message : t(outcome.status === "created" ? "scanSetup.created" : "sites.wizard.creating")}
              </p>}
              <div className="flex flex-wrap items-center justify-between gap-3 border-t border-dashed border-separator pt-3">
                <details className="min-w-0 text-xs text-muted"><summary className="cursor-pointer">{t("siteResume.evidence")}</summary>
                  <ul className="mt-2 space-y-1 [overflow-wrap:anywhere]">{project.evidence.map((value, i) => <li key={i}>{value}</li>)}</ul><p className="mt-2 leading-relaxed">{project.runHint}</p>
                </details>
                <Button size="sm" variant="secondary" disabled={disabled || !selectable(project)} onClick={() => configure(project)}>{t("scanSetup.configure")}</Button>
              </div>
            </article>;
          })}</div>
          {pages > 1 && <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted"><span>{t("siteFiles.page").replace("{page}", String(currentPage)).replace("{total}", String(pages))}</span>
            <div className="flex gap-2"><Button variant="secondary" size="sm" disabled={currentPage === 1} onClick={() => setPage(currentPage - 1)}>{t("siteFiles.previous")}</Button><Button variant="secondary" size="sm" disabled={currentPage === pages} onClick={() => setPage(currentPage + 1)}>{t("siteFiles.next")}</Button></div>
          </div>}
        </>}
        {found?.length === 0 && <div className="space-y-2 rounded-xl bg-fill p-5 text-xs text-muted"><p>{t("scanner.noneFound")}</p><p>{t("scanner.noneFoundHint")}</p></div>}
        {found === null && !busy && !error && <div className="space-y-2 py-6 text-center text-xs text-muted"><p>{t("scanner.idle")}</p><p>{t("scanner.idleHint")}</p></div>}
      </div>
      <div className="mx-4 shrink-0 space-y-2 border-t border-dashed border-separator py-4 sm:mx-5">
        {progress && <p role="status" className="text-xs text-muted [overflow-wrap:anywhere]">{t("scanSetup.progress").replace("{done}", String(progress.done)).replace("{total}", String(progress.total)).replace("{name}", progress.name)}</p>}
        {report && <p role="status" className="text-xs leading-relaxed text-muted">{t("scanSetup.report").replace("{ok}", String(report.ok)).replace("{fail}", String(report.fail))}</p>}
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-xs text-muted">{t("scanner.selected").replace("{n}", String(selected.length))}</span>
          <div className="flex flex-wrap gap-2"><Button variant="ghost" disabled={busy} onClick={() => onOpenChange(false)}>{t("common.close")}</Button>
            <Button disabled={busy || !selected.length || !webServer || !webServers.includes(webServer as "nginx" | "apache") || !!queryError} onClick={() => void createAll()}>{mode === "creating" && <Loader2 className="size-4 animate-spin motion-reduce:animate-none" />}{t("scanner.createN").replace("{n}", String(selected.length))}</Button>
          </div>
        </div>
      </div>
    </DialogContent>
  </Dialog>;
}
