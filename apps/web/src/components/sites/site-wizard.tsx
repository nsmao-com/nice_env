"use client";
import { CustomRewriteSelect } from "./custom-rewrite-select";
import type { CustomRewrite } from "@nsb/schema";
import { isSiteHostname } from "@/lib/utils";


import * as React from "react";
import type { ProjectPhpCompatibility } from "@nsb/schema";
import { ProjectPhpCheck, projectPhpProblem, recommendedProjectPhp } from "./project-php-compatibility";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  Globe,
  FolderOpen,
  Boxes,
  Lock,
  Database,
  Shuffle,
  Check,
  ChevronRight,
  ChevronLeft,
  FileCode2,
  Sparkles,
  ArrowRight,
  Loader2,
} from "lucide-react";
import { RewritePreset as RewriteSchema, type ScannedProject, type CreateSiteInput, type RewritePreset, type SiteKind, type SiteCreateProgress } from "@nsb/schema";
import { cn, cmpVersionDesc, normalizeProxyTarget } from "@/lib/utils";
import { useT, type ExistingProjectDefaults } from "@/lib/store";
import { isTauri, listen, normalizeError } from "@/lib/backend";
import { usePackages, siteUrl, toastError } from "@/lib/hooks";
import { useInstallTasks } from "@/lib/install-tasks";
import * as api from "@/lib/api";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

import { SiteCertificateSelect, useSiteCertificateSelection, type SiteCertificateBinding } from "./site-certificate-select";
import { SiteApplicationFields } from "./site-application-fields";
import { APPLICATION_RUNTIMES, applicationRuntime, validApplication } from "@/lib/utils";

const STEPS = [
  { key: "sites.wizard.step1", icon: Globe },
  { key: "sites.wizard.step2", icon: FolderOpen },
  { key: "sites.wizard.step3", icon: Boxes },
  { key: "sites.wizard.step4", icon: Lock },
  { key: "sites.wizard.step5", icon: Database },
  { key: "sites.wizard.step6", icon: Shuffle },
] as const;

const KINDS: { value: SiteKind; labelKey: string; hintKey: string }[] = [
  { value: "php", labelKey: "wz.kindPhp", hintKey: "wz.kindPhpHint2" },
  { value: "static", labelKey: "wz.kindStatic", hintKey: "wz.staticHint" },
  { value: "reverse-proxy", labelKey: "wz.kindProxy", hintKey: "wz.proxyTargetHint" },
  ...APPLICATION_RUNTIMES.map((runtime) => ({ value: runtime.kind, labelKey: runtime.label, hintKey: "appProcess.kindHint" })),
];

const REWRITES: { value: RewritePreset; label: string }[] = [
  { value: "none", label: "—SKIP—" },
  { value: "laravel", label: "Laravel" },
  { value: "symfony", label: "Symfony" },
  { value: "thinkphp", label: "ThinkPHP" },
  { value: "wordpress", label: "WordPress" },
  { value: "yii2", label: "Yii2" },
  { value: "codeigniter", label: "CodeIgniter 4" },
  { value: "cakephp", label: "CakePHP" },
  { value: "drupal", label: "Drupal" },
  { value: "joomla", label: "Joomla" },
  { value: "spa-fallback", label: "SPA fallback" },
  { value: "next-export", label: "Next.js export" },
];

function generateDatabasePassword() {
  return `nsb_${Array.from(crypto.getRandomValues(new Uint8Array(16)), (value) => value.toString(16).padStart(2, "0")).join("")}`;
}

export function SiteWizard({
  open,
  onOpenChange,
  onCreated,
  initialKind = "php",
  existingProject = null,
  existingDefaults = null,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated?: () => void;
  initialKind?: SiteKind;
  existingProject?: ScannedProject | null;
  existingDefaults?: ExistingProjectDefaults | null;
}) {
  const t = useT();
  const { data: packages, refetch: refreshPackages } = usePackages();
  const [step, setStep] = React.useState(0);
  const [creating, setCreating] = React.useState(false);
  const [installingComposer, setInstallingComposer] = React.useState(false);
  const [installingNode, setInstallingNode] = React.useState(false);
  const [progress, setProgress] = React.useState<SiteCreateProgress | null>(null);
  const [createError, setCreateError] = React.useState("");
  const [createErrorDetail, setCreateErrorDetail] = React.useState("");
  const submitting = React.useRef(false);

  const phpVersions = React.useMemo(
    () =>
      packages
        .filter((p) => p.id === "php" && p.install)
        .map((p) => p.version)
        .sort(cmpVersionDesc),
    [packages]
  );

  const [name, setName] = React.useState("");
  const [domain, setDomain] = React.useState("");
  const [aliases, setAliases] = React.useState("");
  const [rootDir, setRootDir] = React.useState("");
  const [template, setTemplate] = React.useState<CreateSiteInput["template"]>("none");
  const [kind, setKind] = React.useState<SiteKind>("php");
  const [phpVersion, setPhpVersion] = React.useState("");
  const [phpReport, setPhpReport] = React.useState<ProjectPhpCompatibility | null>(null);
  const [phpChecking, setPhpChecking] = React.useState(false);
  const [allowUnverifiedPhp, setAllowUnverifiedPhp] = React.useState(false);
  const [phpCheckRevision, setPhpCheckRevision] = React.useState(0);
  const phpInventory = packages.filter((p) => p.install && (p.id === "php" || p.id === "composer")).map((p) => `${p.id}@${p.version}`).sort().join(",");
  const existingPhpProblem = existingProject && kind === "php" ? projectPhpProblem(phpReport, phpVersion, allowUnverifiedPhp) : null;
  React.useEffect(() => {
    if (!open || !existingProject || kind !== "php") { setPhpChecking(false); return; }
    let cancelled = false;
    setPhpChecking(true); setAllowUnverifiedPhp(false); setPhpReport(null);
    void api.projectPhpCompatibility(existingProject.path).then((report) => {
      if (cancelled) return;
      setPhpReport(report);
      setPhpVersion((current) => current || recommendedProjectPhp(report, phpVersions));
    }).catch((failure) => {
      if (!cancelled) setPhpReport({ status: "unavailable", requirement: existingProject.phpMinVersion ?? null, versions: [], matchingVersions: [], message: normalizeError(failure).message });
    }).finally(() => { if (!cancelled) setPhpChecking(false); });
    return () => { cancelled = true; };
  // Inventory changes require a fresh result; unrelated package polling must not erase the draft.
  }, [open, existingProject?.path, kind, phpInventory, phpCheckRevision]);
  const [webServer, setWebServer] = React.useState<"nginx" | "apache">("nginx");
  const [proxyTarget, setProxyTarget] = React.useState("127.0.0.1:3001");
  const [application, setApplication] = React.useState<CreateSiteInput["runtime"]["application"]>();
  const appRuntime = applicationRuntime(kind);
  const appVersions = packages.filter((p) => p.id === appRuntime?.id && p.install).map((p) => p.version).sort(cmpVersionDesc);
  const applicationValid = validApplication(application, proxyTarget) && (!application || appVersions.includes(application.version));
  const isProxy = kind !== "php" && kind !== "static";
  const normalizedProxyTarget = normalizeProxyTarget(proxyTarget);
  const [https, setHttps] = React.useState(false);
  const [certificate, setCertificate] = React.useState<SiteCertificateBinding>({});
  const certificateSelection = useSiteCertificateSelection(certificate, [domain.trim(), ...aliases.split(/[,，\s]+/).filter(Boolean)], open && https);
  const [dbEnabled, setDbEnabled] = React.useState(false);
  const [dbName, setDbName] = React.useState("");
  const [dbUser, setDbUser] = React.useState("");
  const [dbPass, setDbPass] = React.useState("");
  const [customRewrite, setCustomRewrite] = React.useState<CustomRewrite>();
  React.useEffect(() => setCustomRewrite(undefined), [webServer, kind]);
  const [rewrite, setRewrite] = React.useState<RewritePreset>("none");
  const wasOpen = React.useRef(false);
  const domainEdited = React.useRef(false);
  const webInstalled = packages.some((p) => p.id === webServer && p.install);
  const mysqlInstalled = packages.some((p) => p.id === "mysql" && p.install);
  const composerTemplate = ["laravel", "thinkphp", "symfony", "codeigniter"].includes(template);
  const composerInstalled = packages.some((p) => p.id === "composer" && p.install);
  const nextTemplate = template === "next-export";
  const nodePackages = packages.filter((p) => p.id === "node" && p.install).sort((a, b) => cmpVersionDesc(a.version, b.version));
  const activeNode = nodePackages.find((p) => p.active) ?? nodePackages[0];
  const nodeCompatible = !!activeNode && cmpVersionDesc(activeNode.version, "20.9") <= 0;
  const templateNodeCompatible = !nextTemplate || nodeCompatible;
  const minimumPhp = template === "thinkphp" ? "8.0" : "8.2";
  const templatePhpCompatible = !composerTemplate || (!!phpVersion && cmpVersionDesc(phpVersion, minimumPhp) <= 0);
  const databaseValid = !dbEnabled || (mysqlInstalled && /^[a-z0-9_]{1,64}$/i.test(dbName)
    && /^[a-z0-9_]{1,32}$/i.test(dbUser) && dbPass.length > 0 && !/[\u0000-\u001f\u007f]/.test(dbPass));

  React.useEffect(() => {
    if (open && !wasOpen.current) {
      domainEdited.current = !!existingProject;
      setStep(0);
      setProgress(null);
      setCreateError("");
      setCreateErrorDetail("");
      setName(existingProject?.name ?? "");
      setDomain(existingDefaults?.domain ?? "");
      setAliases("");
      setRootDir(existingProject ? (existingProject.needsDevServer ? existingProject.path : existingProject.documentRoot) : "");
      setTemplate("none");
      setKind(initialKind);
      setPhpVersion(existingDefaults?.phpVersion || (existingProject ? recommendedProjectPhp(existingProject.phpCompatibility, phpVersions) : phpVersions[0]) || "");
      setAllowUnverifiedPhp(false); setPhpReport(existingProject?.phpCompatibility ?? null);
      setWebServer(existingDefaults?.webServer ?? "nginx");
      setHttps(existingDefaults?.https ?? false);
      setCertificate({});
      setDbEnabled(false);
      setRewrite(existingProject && !existingProject.needsDevServer ? RewriteSchema.safeParse(existingProject.rewrite).data ?? "none" : "none");
      setCustomRewrite(undefined);
      setProxyTarget(existingDefaults?.proxyTarget ?? (existingProject?.needsDevServer ? "" : "127.0.0.1:3001"));
      setApplication(undefined);
      setDbName("");
      setDbUser("");
    }
    wasOpen.current = open;
  }, [open, phpVersions, initialKind, existingProject, existingDefaults]);

  React.useEffect(() => {
    if (open && !existingProject && !phpVersion && phpVersions[0]) setPhpVersion(phpVersions[0]);
  }, [open, existingProject, phpVersion, phpVersions]);

  /* 名称 → 域名联动 */
  React.useEffect(() => {
    if (!name) return;
    const slug = name
      .toLowerCase()
      .trim()
      .replace(/[^a-z0-9-]+/g, "-")
      .replace(/^-+|-+$/g, "");
    if (!domainEdited.current) setDomain(`${slug || "myproject"}.test`);
    setDbName(slug.replace(/-/g, "_") || "myproject");
    setDbUser(`${(slug || "user").replace(/-/g, "_")}_user`);
  }, [name]);

  React.useEffect(() => {
    if (open) setDbPass(generateDatabasePassword());
  }, [open]);

  const canNext = React.useMemo(() => {
    if (step >= 2 && kind === "php" && (!phpVersions.includes(phpVersion) || (existingProject && (phpChecking || existingPhpProblem)))) return false;
    if (step >= 3 && https && certificateSelection.problem) return false;
    if (step >= 2 && isProxy && (!normalizedProxyTarget || !applicationValid)) return false;
    switch (step) {
      case 0:
        return name.trim().length > 0 && isSiteHostname(domain) && aliases.split(/[,，\s]+/).filter(Boolean).every(isSiteHostname);
      case 1:
        return rootDir.trim().length > 0 && (!composerTemplate || composerInstalled) && templateNodeCompatible;
      case 2:
        return webInstalled && templatePhpCompatible && (kind !== "php" || phpVersions.includes(phpVersion));
      case 4:
      case 5:
        return databaseValid && (!composerTemplate || composerInstalled) && templatePhpCompatible && templateNodeCompatible;
      default:
        return true;
    }
  }, [existingProject, phpChecking, existingPhpProblem, https, certificateSelection.problem, step, name, domain, aliases, rootDir, kind, phpVersion, phpVersions, webInstalled, normalizedProxyTarget, databaseValid, composerTemplate, composerInstalled, templatePhpCompatible, templateNodeCompatible, isProxy, applicationValid]);

  const pickFolder = async () => {
    if (isTauri) {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({ directory: true, multiple: false, title: t("wz.pickRoot") });
      if (typeof picked === "string") setRootDir(picked);
    } else {
      const v = window.prompt(t("wz.pathHint"), rootDir || "D:/code/my-site");
      if (v) setRootDir(v);
    }
  };

  const installComposer = async () => {
    if (installingComposer) return;
    setInstallingComposer(true);
    try {
      const candidate = packages.filter((p) => p.id === "composer").sort((a, b) => cmpVersionDesc(a.version, b.version))[0];
      const installed = await useInstallTasks.getState().start({ id: "composer", version: candidate?.version, displayName: "Composer" }, { quiet: true });
      if (installed) await refreshPackages();
    } catch (error) {
      toastError(error, t("wz.composerInstallFailed"));
    } finally {
      setInstallingComposer(false);
    }
  };

  const submit = async () => {
    if (submitting.current || !canNext || (isProxy && (!normalizedProxyTarget || !applicationValid)) || (https && certificateSelection.problem)) return;
    submitting.current = true;
    setCreating(true);
    setCreateError("");
    setCreateErrorDetail("");
    setProgress({ rootDir: rootDir.trim(), stage: "preparing", percent: null });
    let unlisten: (() => void) | undefined;
    try {
      const input: CreateSiteInput = {
        name: name.trim(),
        domains: [domain.trim(), ...aliases.split(/[,，\s]+/).filter(Boolean)],
        rootDir: rootDir.trim(),
        runtime: {
          webServer,
          customRewrite,
          kind,
          ...(kind === "php" ? { phpVersion } : {}),
          ...(isProxy ? { proxyTarget: normalizedProxyTarget! } : {}),
          ...(appRuntime && application ? { application } : {}),
          ...(https ? certificate : {}),
        },
        https,
        rewrite,
        ...(dbEnabled && dbName
          ? { createDb: { database: dbName, username: dbUser, password: dbPass } }
          : {}),
        writeEnvExample: !existingProject && dbEnabled,
        template: existingProject ? "none" : template,
      };
      unlisten = await listen<SiteCreateProgress>("site://create-progress", (event) => {
        if (event.rootDir === input.rootDir) setProgress(event);
      });
      const site = await api.createSite(input, existingProject?.path, allowUnverifiedPhp);
      toast.success(`${t("wz.createdP1")} ${site.name} ${t("wz.createdP2")}`, {
        description: template === "wordpress"
          ? t("wz.wordpressFinish")
          : siteUrl(site) ? `${t("wz.visit")} ${siteUrl(site)}` : t("sites.addressPending"),
      });
      onOpenChange(false);
      onCreated?.();
    } catch (e) {
      const error = normalizeError(e);
      setCreateError([error.message, error.hint].filter(Boolean).join(" · "));
      setCreateErrorDetail(error.detail ?? "");
      toastError(e, t("wz.createFailed"));
    } finally {
      unlisten?.();
      submitting.current = false;
      setCreating(false);
    }
  };

  const installNode = async () => {
    if (installingNode) return;
    const candidate = packages.filter((p) => p.id === "node" && cmpVersionDesc(p.version, "20.9") <= 0).sort((a, b) => cmpVersionDesc(a.version, b.version))[0];
    if (!candidate) { toast.error(t("wz.nodeUnavailable")); return; }
    setInstallingNode(true);
    try {
      const installed = await useInstallTasks.getState().start({ id: "node", version: candidate.version, displayName: "Node.js" }, { quiet: true });
      if (installed) await refreshPackages();
    } catch (error) {
      toastError(error, t("wz.nodeInstallFailed"));
    } finally {
      setInstallingNode(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !creating && onOpenChange(o)}>
      <DialogContent className="flex w-[calc(100vw_-_1.5rem)] max-w-[640px] max-h-[86vh] flex-col overflow-hidden">
        <DialogHeader>
          <DialogTitle>{t(existingProject ? "scanSetup.configureTitle" : "sites.create")}</DialogTitle>
          <DialogDescription>
            {t("sites.wizard.step")} {step + 1}/6 · {t(STEPS[step].key)}
          </DialogDescription>
        </DialogHeader>

        {/* 步骤指示器 */}
        <div className="flex items-center gap-1.5">
          {STEPS.map((s, i) => (
            <React.Fragment key={s.key}>
              <div
                className={cn(
                  "flex h-6 w-6 items-center justify-center rounded-full border text-[10px] font-semibold transition-all",
                  i < step
                    ? "border-primary/40 bg-primary-soft text-primary"
                    : i === step
                      ? "border-primary text-primary"
                      : "border-border text-faint"
                )}
              >
                {i < step ? <Check className="h-3 w-3" /> : i + 1}
              </div>
              {i < STEPS.length - 1 && (
                <div className={cn("h-px flex-1 transition-colors", i < step ? "bg-primary/40" : "bg-border")} />
              )}
            </React.Fragment>
          ))}
        </div>

        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto pr-1">
        {existingProject && <p className="mb-4 rounded-lg bg-fill p-3 text-xs leading-relaxed text-muted">{t("siteResume.preserve")}</p>}
        <fieldset disabled={creating} className="m-0 min-w-0 border-0 p-0">
        <AnimatePresence mode="wait">
          <motion.div
            key={step}
            initial={{ opacity: 0, x: 12 }}
            animate={{ opacity: 1, x: 0 }}
            exit={{ opacity: 0, x: -12 }}
            transition={{ duration: 0.18 }}
            className="min-h-[240px]"
          >
            {step === 0 && (
              <div className="flex flex-col gap-4">
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="sw-name">{t("sites.wizard.name")}</Label>
                  <Input
                    id="sw-name"
                    placeholder={t("sites.wizard.namePh")}
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    autoFocus
                  />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="sw-domain">{t("sites.wizard.domain")}</Label>
                  <Input
                    id="sw-domain"
                    placeholder="myproject.test"
                    value={domain}
                    onChange={(e) => { domainEdited.current = true; setDomain(e.target.value); }}
                    className="font-mono text-[13px]"
                  />
                  <p className="text-[11px] text-faint">{t(existingProject ? "siteResume.domain" : "sites.wizard.domainHint")}</p>
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="sw-aliases">{t("sites.wizard.aliases")}</Label>
                  <Input
                    id="sw-aliases"
                    placeholder="www.myproject.test, api.myproject.test"
                    value={aliases}
                    onChange={(e) => setAliases(e.target.value)}
                  />
                </div>
              </div>
            )}

            {step === 1 && (
              <div className="flex flex-col gap-4">
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="sw-root">{t("sites.wizard.root")}</Label>
                  <div className="flex gap-2">
                    <Input
                      id="sw-root"
                      readOnly={!!existingProject}
                      value={rootDir}
                      onChange={(e) => setRootDir(e.target.value)}
                      placeholder="D:/code/my-site"
                      className="flex-1 font-mono text-[13px]"
                    />
                    <Button variant="secondary" onClick={pickFolder} disabled={!!existingProject} className="shrink-0">
                      <FolderOpen className="h-3.5 w-3.5" /> {t("sites.wizard.pick")}
                    </Button>
                  </div>
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label>{t("sites.wizard.template")}</Label>
                  <div className="grid grid-cols-2 gap-2">
                    {(
                      [
                        { v: "none", labelKey: "wz.useExistingOpt", hintKey: "wz.existingCodeHint" },
                        { v: "blank-php", labelKey: "wz.blankPhp", hintKey: "wz.tplBlankPhpHint2" },
                        { v: "laravel", labelKey: "wz.laravelOpt", hintKey: "wz.laravelOptHint" },
                        { v: "thinkphp", labelKey: "wz.tplThinkPhp", hintKey: "wz.tplThinkPhpHint" },
                        { v: "wordpress", labelKey: "wz.tplWordPress", hintKey: "wz.tplWordPressHint" },
                        { v: "symfony", labelKey: "wz.tplSymfony", hintKey: "wz.tplSymfonyHint" },
                        { v: "codeigniter", labelKey: "wz.tplCodeIgniter", hintKey: "wz.tplCodeIgniterHint" },
                        { v: "static", labelKey: "wz.staticHome", hintKey: "wz.staticOptHint" },
                        { v: "spa", labelKey: "wz.tplSpa", hintKey: "wz.tplSpaHint" },
                        { v: "next-export", labelKey: "wz.tplNextExport", hintKey: "wz.tplNextExportHint" },
                      ] as const
                    ).filter((opt) => !existingProject || opt.v === "none").map((opt) => (
                      <button
                        key={opt.v}
                        disabled={!!existingProject}
                        onClick={() => {
                          setTemplate(opt.v);
                          if (opt.v === "next-export") setDbEnabled(false);
                          if (["static", "spa", "next-export"].includes(opt.v)) setKind("static");
                          else if (opt.v !== "none") setKind("php");
                          // 模板与伪静态是强绑定的：选了 Laravel 就该给 laravel 规则。
                          // 让用户再手动选一次纯属多余，而且选错就 404。
                          const auto: Record<string, RewritePreset> = {
                            laravel: "laravel",
                            thinkphp: "thinkphp",
                            wordpress: "wordpress",
                            symfony: "symfony",
                            codeigniter: "codeigniter",
                            "next-export": "next-export",
                            spa: "spa-fallback",
                            static: "none",
                            "blank-php": "none",
                            none: "none",
                          };
                          const r = auto[opt.v];
                          if (r) setRewrite(r);
                        }}
                        className={cn(
                          "flex flex-col items-start gap-1 rounded-xl border p-3 text-left transition-all",
                          template === opt.v
                            ? "border-primary/60 bg-primary-soft"
                            : "border-border hover:border-border-strong bg-card-2/40"
                        )}
                      >
                        <span className="flex items-center gap-1.5 text-[12.5px] font-medium">
                          <FileCode2 className={cn("h-3.5 w-3.5", template === opt.v ? "text-primary" : "text-faint")} />
                          {t(opt.labelKey as never)}
                        </span>
                        <span className="text-[11px] text-faint">{t(opt.hintKey as never)}</span>
                      </button>
                    ))}
                  </div>
                  {template === "wordpress" && (
                    <p className="text-xs leading-relaxed text-muted">{t("wz.wordpressInstallHint")}</p>
                  )}
                  {nextTemplate && (
                    <div className={cn("flex flex-col gap-2 rounded-lg p-3 text-xs leading-relaxed", nodeCompatible ? "bg-card-2 text-muted" : "bg-warn-soft text-warn")}>
                      <p>{t("wz.nextExportInstallHint")}</p>
                      <p role={nodeCompatible ? undefined : "alert"}>{activeNode
                        ? t(nodeCompatible ? "wz.nextNodeSelected" : "wz.nextNodeTooOld").replace("{version}", activeNode.version)
                        : t("wz.installNodeFirst")}</p>
                      {!activeNode && <Button variant="secondary" size="sm" className="self-start" onClick={installNode} disabled={installingNode}>
                        {installingNode && <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />}{t(installingNode ? "wz.nodeInstalling" : "wz.installNode")}
                      </Button>}
                    </div>
                  )}
                  {composerTemplate && (
                    <div className={cn("flex flex-col gap-2 rounded-lg p-3 text-xs leading-relaxed", composerInstalled ? "bg-card-2 text-muted" : "bg-warn-soft text-warn")}>
                      <p role={composerInstalled ? undefined : "alert"}>{t(composerInstalled ? "wz.composerInstallHint" : "wz.installComposerFirst")}</p>
                      {!composerInstalled && <Button variant="secondary" size="sm" className="self-start" onClick={installComposer} disabled={installingComposer}>
                        {installingComposer && <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />}{t(installingComposer ? "wz.composerInstalling" : "wz.installComposer")}
                      </Button>}
                    </div>
                  )}
                </div>
              </div>
            )}

            {step === 2 && (
              <div className="flex flex-col gap-4">
                {existingProject && kind === "php" && <ProjectPhpCheck report={phpReport} version={phpVersion} acknowledged={allowUnverifiedPhp} loading={phpChecking} disabled={creating}
                  onAcknowledge={setAllowUnverifiedPhp} onRefresh={() => setPhpCheckRevision((value) => value + 1)} />}
                {existingProject?.phpMinVersion && <p className="rounded-lg bg-fill p-3 text-xs text-muted">{t("siteResume.php").replace("{version}", existingProject.phpMinVersion)}</p>}
                {existingProject?.needsDevServer && <p className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t("siteResume.application")}</p>}
                {!templatePhpCompatible && <p role="alert" className="rounded-lg bg-warn-soft p-3 text-xs text-warn">{t("wz.templatePhpMinimum").replace("{version}", minimumPhp)}</p>}
                {!webInstalled && <p role="alert" className="rounded-lg bg-warn-soft p-3 text-xs text-warn">{t("wz.installWebFirst")}</p>}
                <div className="flex flex-col gap-1.5">
                  <Label>{t("sites.wizard.kind")}</Label>
                  <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                    {KINDS.map((k) => (
                      <button
                        key={k.value}
                        aria-pressed={kind === k.value}
                        disabled={!!existingProject && k.value !== initialKind}
                        onClick={() => {
                          setKind(k.value);
                          if (k.value !== kind) setApplication(undefined);
                          const staticTemplate = ["static", "spa", "next-export"].includes(template);
                          if ((k.value !== "php" && k.value !== "static") || (template !== "none" && (staticTemplate !== (k.value === "static")))) {
                            setTemplate("none"); setRewrite("none");
                          }
                        }}
                        className={cn(
                          "flex flex-col items-start gap-0.5 rounded-xl border p-3 text-left transition-all",
                          kind === k.value
                            ? "border-primary/60 bg-primary-soft"
                            : "border-border hover:border-border-strong bg-card-2/40"
                        )}
                      >
                        <span className="text-[12.5px] font-medium">{applicationRuntime(k.value)?.label ?? t(k.labelKey as never)}</span>
                        <span className="text-[11px] text-faint">{t(k.hintKey as never)}</span>
                      </button>
                    ))}
                  </div>
                </div>
                {kind === "php" && (
                  <div className="flex flex-col gap-1.5">
                    <Label>{t("sites.wizard.phpVersion")}</Label>
                    {phpVersions.length === 0 ? (
                      <p className="rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-[11.5px] text-warn">
                        {t("wz.noPhpHint")}
                      </p>
                    ) : (
                      <div className="flex flex-wrap gap-2">
                        {phpVersions.map((v) => (
                          <button
                            key={v}
                            onClick={() => { setPhpVersion(v); setAllowUnverifiedPhp(false); }}
                            aria-pressed={phpVersion === v}
                            className={cn(
                              "rounded-lg border px-3 py-1.5 font-mono text-[12px] transition-all",
                              phpVersion === v
                                ? "border-primary/60 bg-primary-soft text-primary"
                                : "border-border text-muted hover:border-border-strong"
                            )}
                          >
                            PHP {v}
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                )}
                <div className="flex flex-col gap-1.5">
                  <Label>{t("sites.wizard.webServer")}</Label>
                  <div className="flex gap-2">
                    {[
                      { v: "nginx", label: "Nginx", hintKey: "wz.nginxHint" },
                      { v: "apache", label: "Apache", hintKey: "wz.apacheHint" },
                    ].map((w) => (
                      <button
                        key={w.v}
                        aria-pressed={webServer === w.v}
                        onClick={() => setWebServer(w.v as "nginx" | "apache")}
                        className={cn(
                          "flex flex-1 flex-col items-start gap-0.5 rounded-xl border p-3 text-left transition-all",
                          webServer === w.v
                            ? "border-primary/60 bg-primary-soft"
                            : "border-border hover:border-border-strong bg-card-2/40"
                        )}
                      >
                        <span className="text-[12.5px] font-medium">{w.label}</span>
                        <span className="text-[11px] text-faint">{t(w.hintKey as never)}</span>
                      </button>
                    ))}
                  </div>
                </div>
                {isProxy && (
                  <div className="flex flex-col gap-1.5">
                    <Label htmlFor="site-create-proxy">{t("sites.wizard.proxyTarget")}</Label>
                    <Input
                      id="site-create-proxy"
                      value={proxyTarget}
                      onChange={(e) => setProxyTarget(e.target.value)}
                      placeholder="127.0.0.1:8080"
                      className="font-mono text-[13px]"
                      spellCheck={false}
                      autoCapitalize="none"
                      aria-invalid={!normalizedProxyTarget}
                      aria-describedby="site-create-proxy-hint site-create-proxy-result"
                    />
                    <p id="site-create-proxy-hint" className="text-xs leading-relaxed text-muted">{t("wz.proxyHint")}</p>
                    <p id="site-create-proxy-result" aria-live="polite" className={cn("text-xs leading-relaxed [overflow-wrap:anywhere]", normalizedProxyTarget ? "text-muted" : "text-error")}>
                      {normalizedProxyTarget ? t("sites.proxy.effectiveTarget").replace("{target}", normalizedProxyTarget) : t("sites.proxy.invalidTarget")}
                    </p>
                  </div>
                )}
                {appRuntime && <SiteApplicationFields id="site-create-app" kind={kind} value={application} versions={appVersions} rootDir={rootDir}
                  disabled={creating} onChange={setApplication} />}
                {!applicationValid && <p role="alert" className="text-xs leading-relaxed text-error">{t("appProcess.invalid")}</p>}
              </div>
            )}

            {step === 3 && (
              <div className="flex flex-col gap-4">
                <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-4">
                  <div className="flex flex-col gap-0.5">
                    <span className="flex items-center gap-2 text-[13px] font-medium">
                      <Lock className="h-3.5 w-3.5 text-primary" /> {t("sites.wizard.https")}
                    </span>
                    <span className="text-[11.5px] text-faint">{t("sites.detail.httpsSelectHint")}</span>
                  </div>
                  <Switch checked={https} onCheckedChange={setHttps} aria-label={t("sites.wizard.https")} />
                </div>
                {https && (
                  <div className="min-w-0 rounded-xl border border-border p-4">
                    <SiteCertificateSelect id="sw-cert-source" selection={certificateSelection} onChange={setCertificate} disabled={creating} />
                  </div>
                )}
              </div>
            )}

            {step === 4 && (
              <div className="flex flex-col gap-4">
                {nextTemplate ? <p className="rounded-xl bg-fill p-4 text-sm leading-relaxed text-muted">{t("wz.nextExportNoDb")}</p> : <>
                <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-4">
                  <div className="flex flex-col gap-0.5">
                    <span className="flex items-center gap-2 text-[13px] font-medium">
                      <Database className="h-3.5 w-3.5 text-primary" /> {t("sites.wizard.db")}
                    </span>
                    <span className="text-[11.5px] text-faint">{t(existingProject ? "siteResume.database" : "sites.wizard.envHint")}</span>
                  </div>
                  <Switch checked={dbEnabled} onCheckedChange={setDbEnabled} aria-label={t("sites.wizard.db")} />
                </div>
                {dbEnabled && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    className="flex flex-col gap-3 overflow-hidden"
                  >
                    <div className="grid grid-cols-2 gap-3">
                      <div className="flex flex-col gap-1.5">
                        <Label htmlFor="sw-db-name">{t("sites.wizard.dbName")}</Label>
                        <Input id="sw-db-name" value={dbName} onChange={(e) => setDbName(e.target.value)} maxLength={64} className="font-mono text-[13px]" />
                      </div>
                      <div className="flex flex-col gap-1.5">
                        <Label htmlFor="sw-db-user">{t("sites.wizard.dbUser")}</Label>
                        <Input id="sw-db-user" value={dbUser} onChange={(e) => setDbUser(e.target.value)} maxLength={32} className="font-mono text-[13px]" />
                      </div>
                    </div>
                    <div className="flex flex-col gap-1.5">
                      <Label htmlFor="sw-db-pass">{t("sites.wizard.dbPass")}</Label>
                      <div className="flex gap-2">
                        <Input id="sw-db-pass" value={dbPass} onChange={(e) => setDbPass(e.target.value)} className="min-w-0 flex-1 font-mono text-[13px]" />
                        <Button variant="ghost" size="icon" onClick={() => setDbPass(generateDatabasePassword())} title={t("wz.regenerate")} aria-label={t("wz.regenerate")}>
                          <Sparkles className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </div>
                  </motion.div>
                )}
                {dbEnabled && !databaseValid && (
                  <p role="alert" className="rounded-lg bg-warn-soft p-3 text-xs leading-relaxed text-warn">{t(mysqlInstalled ? "wz.databaseValidation" : "wz.installMysqlFirst")}</p>
                )}
                </>}
              </div>
            )}

            {step === 5 && (
              <div className="flex flex-col gap-4">
                <div className="flex flex-col gap-1.5">
                  <Label>{t("sites.wizard.rewrite")}</Label>
                  <Select value={rewrite} onValueChange={(v) => { setRewrite(v as RewritePreset); setCustomRewrite(undefined); }}>
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {REWRITES.map((r) => (
                        <SelectItem key={r.value} value={r.value}>
                          {r.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                {(kind === "php" || kind === "static") && <CustomRewriteSelect server={webServer} value={customRewrite} onChange={setCustomRewrite} />}
                {/* 摘要 */}
                <div className="flex flex-col gap-2 rounded-xl bg-fill p-4 text-[12px]">
                  <SummaryRow label={t("wz.domain")} value={[domain, ...aliases.split(/[,，\s]+/).filter(Boolean)].join(" · ")} mono />
                  <SummaryRow label={t("wz.rootDir")} value={rootDir} mono />
                  <SummaryRow
                    label={t("wz.runtime")}
                    value={
                      kind === "php"
                        ? `${webServer === "apache" ? "Apache" : "Nginx"} + PHP ${phpVersion || t("wz.phpPending")}`
                        : isProxy
                          ? `${webServer === "apache" ? "Apache" : "Nginx"} ${t("sites.proxyP1")} ${normalizedProxyTarget ?? proxyTarget}`
                          : `${webServer === "apache" ? "Apache" : "Nginx"} · ${t("sites.static")}`
                    }
                  />
                  {appRuntime && <SummaryRow label={t("appProcess.title")} value={application ? `${appRuntime.label} ${application.version} · ${application.args.join(" · ")}` : t("appProcess.externalHint")} />}
                  <SummaryRow label="HTTPS" value={https ? certificateSelection.value === "local" ? t("wz.caAuto") : certificateSelection.selected?.subject ?? t("sites.detail.certUnavailableSelection") : t("detail.none")} />
                  <SummaryRow label={t("wz.db")} value={dbEnabled ? `${dbName}（${dbUser}）` : t("wz.noDb")} />
                  <SummaryRow label={t("wz.rewrite")} value={customRewrite ? customRewrite.name : rewrite === "none" ? t("wz.noneOpt") : REWRITES.find((r) => r.value === rewrite)?.label ?? ""} />
                </div>
                <Badge variant="info" className="w-fit">
                  <ArrowRight className="h-3 w-3" /> {t("wz.createHint")}
                </Badge>
              </div>
            )}
          </motion.div>
        </AnimatePresence>
        </fieldset>
        </div>

        {creating && progress && (
          <div role="status" aria-live="polite" className="flex shrink-0 items-center gap-2 rounded-lg bg-card-2 p-3 text-xs text-secondary">
            <Loader2 className="h-4 w-4 shrink-0 animate-spin motion-reduce:animate-none" />
            <span>{t(`wz.progress.${progress.stage}`)}{progress.percent !== null ? ` ${progress.percent}%` : ""}</span>
          </div>
        )}
        {!creating && step > 2 && existingProject && kind === "php" && (phpChecking || existingPhpProblem || !phpVersions.includes(phpVersion)) &&
          <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-warn-soft p-3 text-xs text-warn">
            <span>{t(phpChecking ? "projectPhp.checking" : !phpVersions.includes(phpVersion) ? "scanSetup.phpRequired" : existingPhpProblem!)}</span>
            <Button size="sm" variant="ghost" onClick={() => setStep(2)}>{t("projectPhp.configure")}</Button>
          </div>}
        {!creating && createError && (
          <div className="max-h-40 shrink-0 overflow-y-auto break-words rounded-lg bg-error-soft p-3 text-xs leading-relaxed text-error">
            <p role="alert">{createError}</p>
            {createErrorDetail && <details className="mt-2"><summary className="cursor-pointer font-medium">{t("wz.errorDetails")}</summary><pre className="mt-2 whitespace-pre-wrap break-all font-mono text-[11px]">{createErrorDetail}</pre></details>}
          </div>
        )}
        {step > 3 && https && certificateSelection.problem && (
          <div role="alert" className="flex shrink-0 flex-wrap items-center gap-2 rounded-lg bg-error-soft p-3 text-xs text-error">
            <span className="min-w-0 break-all">{certificateSelection.problem}</span>
            <Button variant="ghost" size="sm" disabled={creating} onClick={() => setStep(3)}>{t("sites.detail.certSource")}</Button>
          </div>
        )}

        <div className="flex shrink-0 items-center justify-between border-t border-dashed border-separator pt-4">
          <Button variant="ghost" onClick={() => setStep((s) => Math.max(0, s - 1))} disabled={step === 0 || creating}>
            <ChevronLeft className="h-3.5 w-3.5" /> {t("common.back")}
          </Button>
          {step < STEPS.length - 1 ? (
            <Button onClick={() => setStep((s) => s + 1)} disabled={!canNext}>
              {t("common.next")} <ChevronRight className="h-3.5 w-3.5" />
            </Button>
          ) : (
            <Button onClick={submit} disabled={!canNext || creating}>
              {creating ? t("sites.wizard.creating") : <>{t("sites.wizard.finish")} <Check className="h-3.5 w-3.5" /></>}
            </Button>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function SummaryRow({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex items-start justify-between gap-4">
      <span className="shrink-0 text-faint">{label}</span>
      <span className={cn("min-w-0 text-right text-secondary [overflow-wrap:anywhere]", mono && "font-mono text-[11.5px]")}>{value}</span>
    </div>
  );
}
