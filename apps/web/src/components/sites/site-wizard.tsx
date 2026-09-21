"use client";

import * as React from "react";
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
} from "lucide-react";
import type { CreateSiteInput, RewritePreset, SiteKind } from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useUI, useT } from "@/lib/store";
import { isTauri } from "@/lib/backend";
import { usePackages, toastError } from "@/lib/hooks";
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
];

const REWRITES: { value: RewritePreset; label: string }[] = [
  { value: "none", label: "—SKIP—" },
  { value: "laravel", label: "Laravel / Symfony" },
  { value: "thinkphp", label: "ThinkPHP" },
  { value: "wordpress", label: "WordPress" },
  { value: "spa-fallback", label: "SPA fallback" },
  { value: "next-export", label: "Next.js export" },
];

export function SiteWizard({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated?: () => void;
}) {
  const t = useT();
  const { data: packages } = usePackages();
  const [step, setStep] = React.useState(0);
  const [creating, setCreating] = React.useState(false);

  const phpVersions = React.useMemo(
    () =>
      packages
        .filter((p) => p.id === "php" && p.install)
        .map((p) => p.version)
        .sort()
        .reverse(),
    [packages]
  );

  const [name, setName] = React.useState("");
  const [domain, setDomain] = React.useState("");
  const [aliases, setAliases] = React.useState("");
  const [rootDir, setRootDir] = React.useState("");
  const [template, setTemplate] = React.useState<CreateSiteInput["template"]>("none");
  const [kind, setKind] = React.useState<SiteKind>("php");
  const [phpVersion, setPhpVersion] = React.useState("");
  const [webServer, setWebServer] = React.useState<"nginx" | "apache">("nginx");
  const [proxyTarget, setProxyTarget] = React.useState("127.0.0.1:8080");
  const [https, setHttps] = React.useState(false);
  const [dbEnabled, setDbEnabled] = React.useState(false);
  const [dbName, setDbName] = React.useState("");
  const [dbUser, setDbUser] = React.useState("");
  const [dbPass, setDbPass] = React.useState("");
  const [rewrite, setRewrite] = React.useState<RewritePreset>("none");

  React.useEffect(() => {
    if (open) {
      setStep(0);
      setName("");
      setDomain("");
      setAliases("");
      setRootDir("");
      setTemplate("none");
      setKind("php");
      setPhpVersion(phpVersions[0] ?? "");
      setWebServer("nginx");
      setHttps(false);
      setDbEnabled(false);
      setRewrite("none");
    }
  }, [open, phpVersions]);

  /* 名称 → 域名联动 */
  React.useEffect(() => {
    if (!name) return;
    const slug = name
      .toLowerCase()
      .trim()
      .replace(/[^a-z0-9-]+/g, "-")
      .replace(/^-+|-+$/g, "");
    setDomain(`${slug || "myproject"}.test`);
    setDbName(slug.replace(/-/g, "_") || "myproject");
    setDbUser(`${(slug || "user").replace(/-/g, "_")}_user`);
  }, [name]);

  React.useEffect(() => {
    setDbPass(`nsb_${Math.random().toString(36).slice(2, 10)}`);
  }, [open]);

  const canNext = React.useMemo(() => {
    switch (step) {
      case 0:
        return name.trim().length > 0 && /^[a-z0-9.-]+\.[a-z]{2,}$/i.test(domain.trim());
      case 1:
        return rootDir.trim().length > 0;
      case 2:
        return kind !== "php" || phpVersions.length === 0 || !!phpVersion;
      default:
        return true;
    }
  }, [step, name, domain, rootDir, kind, phpVersion, phpVersions.length]);

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

  const submit = async () => {
    setCreating(true);
    try {
      const input: CreateSiteInput = {
        name: name.trim(),
        domains: [domain.trim(), ...aliases.split(/[,，\s]+/).filter(Boolean)],
        rootDir: rootDir.trim(),
        runtime: {
          webServer,
          kind,
          ...(kind === "php" ? { phpVersion } : {}),
          ...(kind === "reverse-proxy" ? { proxyTarget } : {}),
        },
        https,
        rewrite,
        ...(dbEnabled && dbName
          ? { createDb: { database: dbName, username: dbUser, password: dbPass } }
          : {}),
        writeEnvExample: dbEnabled,
        template,
      };
      const site = await api.createSite(input);
      toast.success(`${t("wz.createdP1")} ${site.name} ${t("wz.createdP2")}`, {
        description: `${t("wz.visit")} ${site.https ? "https" : "http"}://${site.domains[0]}`,
      });
      onOpenChange(false);
      onCreated?.();
    } catch (e) {
      toastError(e, t("wz.createFailed"));
    } finally {
      setCreating(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !creating && onOpenChange(o)}>
      <DialogContent className="max-w-[560px] max-h-[86vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("sites.create")}</DialogTitle>
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
                    onChange={(e) => setDomain(e.target.value)}
                    className="font-mono text-[13px]"
                  />
                  <p className="text-[11px] text-faint">{t("sites.wizard.domainHint")}</p>
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
                  <Label>{t("sites.wizard.root")}</Label>
                  <div className="flex gap-2">
                    <Input
                      value={rootDir}
                      onChange={(e) => setRootDir(e.target.value)}
                      placeholder="D:/code/my-site"
                      className="flex-1 font-mono text-[13px]"
                    />
                    <Button variant="secondary" onClick={pickFolder} className="shrink-0">
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
                    ).map((opt) => (
                      <button
                        key={opt.v}
                        onClick={() => {
                          setTemplate(opt.v);
                          // 模板与伪静态是强绑定的：选了 Laravel 就该给 laravel 规则。
                          // 让用户再手动选一次纯属多余，而且选错就 404。
                          const auto: Record<string, RewritePreset> = {
                            laravel: "laravel",
                            thinkphp: "thinkphp",
                            wordpress: "wordpress",
                            symfony: "laravel",
                            codeigniter: "none",
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
                </div>
              </div>
            )}

            {step === 2 && (
              <div className="flex flex-col gap-4">
                <div className="flex flex-col gap-1.5">
                  <Label>{t("sites.wizard.kind")}</Label>
                  <div className="flex flex-col gap-2">
                    {KINDS.map((k) => (
                      <button
                        key={k.value}
                        onClick={() => setKind(k.value)}
                        className={cn(
                          "flex flex-col items-start gap-0.5 rounded-xl border p-3 text-left transition-all",
                          kind === k.value
                            ? "border-primary/60 bg-primary-soft"
                            : "border-border hover:border-border-strong bg-card-2/40"
                        )}
                      >
                        <span className="text-[12.5px] font-medium">{t(k.labelKey as never)}</span>
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
                            onClick={() => setPhpVersion(v)}
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
                {(kind === "php" || kind === "static") && (
                  <div className="flex flex-col gap-1.5">
                    <Label>{t("sites.wizard.webServer")}</Label>
                    <div className="flex gap-2">
                      {[
                        { v: "nginx", label: t("wz.staticNginx"), hintKey: "wz.nginxHint" },
                        { v: "apache", label: "Apache", hintKey: "wz.apacheHint" },
                      ].map((w) => (
                        <button
                          key={w.v}
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
                )}
                {kind === "reverse-proxy" && (
                  <div className="flex flex-col gap-1.5">
                    <Label>{t("sites.wizard.proxyTarget")}</Label>
                    <Input
                      value={proxyTarget}
                      onChange={(e) => setProxyTarget(e.target.value)}
                      placeholder="127.0.0.1:8080"
                      className="font-mono text-[13px]"
                    />
                    <p className="text-[11px] text-faint">{t("wz.proxyHint")}</p>
                  </div>
                )}
              </div>
            )}

            {step === 3 && (
              <div className="flex flex-col gap-4">
                <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-4">
                  <div className="flex flex-col gap-0.5">
                    <span className="flex items-center gap-2 text-[13px] font-medium">
                      <Lock className="h-3.5 w-3.5 text-primary" /> {t("sites.wizard.https")}
                    </span>
                    <span className="text-[11.5px] text-faint">{t("sites.wizard.httpsHint")}</span>
                  </div>
                  <Switch checked={https} onCheckedChange={setHttps} />
                </div>
                {https && (
                  <div className="rounded-xl border border-info/25 bg-info-soft px-4 py-3 text-[11.5px] text-info">
                    {t("wz.httpsHint")} <b className="font-mono">{domain}</b>
                    {t("wz.trustOnceHint")}
                  </div>
                )}
              </div>
            )}

            {step === 4 && (
              <div className="flex flex-col gap-4">
                <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-4">
                  <div className="flex flex-col gap-0.5">
                    <span className="flex items-center gap-2 text-[13px] font-medium">
                      <Database className="h-3.5 w-3.5 text-primary" /> {t("sites.wizard.db")}
                    </span>
                    <span className="text-[11.5px] text-faint">{t("sites.wizard.envHint")}</span>
                  </div>
                  <Switch checked={dbEnabled} onCheckedChange={setDbEnabled} />
                </div>
                {dbEnabled && (
                  <motion.div
                    initial={{ opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    className="flex flex-col gap-3 overflow-hidden"
                  >
                    <div className="grid grid-cols-2 gap-3">
                      <div className="flex flex-col gap-1.5">
                        <Label>{t("sites.wizard.dbName")}</Label>
                        <Input value={dbName} onChange={(e) => setDbName(e.target.value)} className="font-mono text-[13px]" />
                      </div>
                      <div className="flex flex-col gap-1.5">
                        <Label>{t("sites.wizard.dbUser")}</Label>
                        <Input value={dbUser} onChange={(e) => setDbUser(e.target.value)} className="font-mono text-[13px]" />
                      </div>
                    </div>
                    <div className="flex flex-col gap-1.5">
                      <Label>{t("sites.wizard.dbPass")}</Label>
                      <div className="flex gap-2">
                        <Input value={dbPass} onChange={(e) => setDbPass(e.target.value)} className="flex-1 font-mono text-[13px]" />
                        <Button variant="ghost" size="icon" onClick={() => setDbPass(`nsb_${Math.random().toString(36).slice(2, 10)}`)} title={t("wz.regenerate")}>
                          <Sparkles className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </div>
                  </motion.div>
                )}
              </div>
            )}

            {step === 5 && (
              <div className="flex flex-col gap-4">
                <div className="flex flex-col gap-1.5">
                  <Label>{t("sites.wizard.rewrite")}</Label>
                  <Select value={rewrite} onValueChange={(v) => setRewrite(v as RewritePreset)}>
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
                {/* 摘要 */}
                <div className="flex flex-col gap-2 rounded-xl bg-fill p-4 text-[12px]">
                  <SummaryRow label={t("wz.domain")} value={[domain, ...aliases.split(/[,，\s]+/).filter(Boolean)].join(" · ")} mono />
                  <SummaryRow label={t("wz.rootDir")} value={rootDir} mono />
                  <SummaryRow
                    label={t("wz.runtime")}
                    value={
                      kind === "php"
                        ? `Nginx + PHP ${phpVersion || t("wz.phpPending")}`
                        : kind === "reverse-proxy"
                          ? `Nginx ${t("sites.proxyP1")} ${proxyTarget}`
                          : t("wz.staticNginx")
                    }
                  />
                  <SummaryRow label="HTTPS" value={https ? t("wz.caAuto") : t("detail.none")} />
                  <SummaryRow label={t("wz.db")} value={dbEnabled ? `${dbName}（${dbUser}）` : t("wz.noDb")} />
                  <SummaryRow label={t("wz.rewrite")} value={rewrite === "none" ? t("wz.noneOpt") : REWRITES.find((r) => r.value === rewrite)?.label ?? ""} />
                </div>
                <Badge variant="info" className="w-fit">
                  <ArrowRight className="h-3 w-3" /> {t("wz.createHint")}
                </Badge>
              </div>
            )}
          </motion.div>
        </AnimatePresence>

        <div className="flex items-center justify-between">
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
      <span className={cn("truncate text-right text-secondary", mono && "font-mono text-[11.5px]")}>{value}</span>
    </div>
  );
}
