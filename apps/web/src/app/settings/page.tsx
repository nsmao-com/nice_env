"use client";

import * as React from "react";
import { toast } from "sonner";
import { useTheme } from "next-themes";
import {
  Palette,
  Sun,
  Moon,
  Monitor,
  Settings2,
  Rocket,
  FolderTree,
  Download,
  RefreshCw,
  Accessibility,
  Archive,
  Upload,
  HardDriveDownload,
  Network,
  ScrollText,
  ShieldAlert,
  FileJson,
  Type,
  SlidersHorizontal,
  RotateCcw,
  Pencil,
  StretchHorizontal,
  Server,
  Check,
  Globe,
  Activity,
} from "lucide-react";
import { isTauri } from "@/lib/backend";
import {
  ACCENT_PRESETS,
  CODE_THEME_OPTIONS,
  MONO_FONT_OPTIONS,
  UI_FONT_OPTIONS,
  type AppSettings,
} from "@nsb/schema";
import { cn } from "@/lib/utils";
import { useUI, useT } from "@/lib/store";
import { toastError, useStacks } from "@/lib/hooks";
import * as api from "@/lib/api";
import { applyAppearance, matchingPreset, effectiveHue, hslToHex } from "@/lib/appearance";
import { detectLocalFonts, scanLocalFonts as scanFonts } from "@/lib/fonts";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { CodeBlock } from "@/components/shared/code-block";
import { CopyButton, ConfirmDialog } from "@/components/shared/misc";
import { ToolMirrorCard } from "@/components/shared/tool-mirror-card";
import { UpdateDialog } from "@/components/shared/update-dialog";
import { PageHeader } from "@/components/layout/app-shell";

/** 可逐个覆盖的端口项（key 与 Rust PortsProfile 字段一一对应） */
const PORT_FIELDS = [
  { key: "http", label: "Nginx", fallback: { safe: 8080, standard: 80 } },
  { key: "https", label: "Nginx (HTTPS)", fallback: { safe: 8443, standard: 443 } },
  { key: "mysql", label: "MySQL", fallback: { safe: 23306, standard: 3306 } },
  { key: "redis", label: "Redis", fallback: { safe: 26379, standard: 6379 } },
  { key: "postgres", label: "PostgreSQL", fallback: { safe: 25432, standard: 5432 } },
  { key: "mongodb", label: "MongoDB", fallback: { safe: 28017, standard: 27017 } },
  { key: "apacheHttp", label: "Apache", fallback: { safe: 8180, standard: 8080 } },
  { key: "apacheHttps", label: "Apache (HTTPS)", fallback: { safe: 8444, standard: 8443 } },
] as const;

/** 左侧分区导航：设置项多了以后，一屏铺 8 张卡很难扫读 */
const SECTIONS = [
  { id: "appearance", icon: Palette, labelKey: "settings.section.appearance" },
  { id: "general", icon: Settings2, labelKey: "settings.section.general" },
  { id: "services", icon: Server, labelKey: "settings.section.services" },
  { id: "logs", icon: ScrollText, labelKey: "settings.section.logs" },
  { id: "storage", icon: FolderTree, labelKey: "settings.section.storage" },
  { id: "updates", icon: Download, labelKey: "settings.section.updates" },
  { id: "advanced", icon: SlidersHorizontal, labelKey: "settings.section.advanced" },
] as const;

const CODE_SIZES = [10, 11, 11.5, 12, 13, 14, 16];
const UI_SCALES = [0.85, 0.9, 0.95, 1, 1.05, 1.1, 1.15, 1.25];

/**
 * 字体选择：内置预设 + 「扫描本机字体」拿到的系统字体 + 手动输入字体名。
 * 本机字体以 `local:<字体族名>` 存进设置（见 appearance.ts），无权限时也能手填。
 */
function FontSelect({
  value,
  presets,
  localFonts,
  onChange,
  customLabel,
}: {
  value: string;
  presets: readonly { id: string; label: string }[];
  localFonts: string[];
  onChange: (v: string) => void;
  customLabel: string;
}) {
  const isLocal = value.startsWith("local:");
  const [customOpen, setCustomOpen] = React.useState(false);
  const t = useT();
  // 重启应用后扫描列表还没回来：把当前值临时注入，保证下拉框显示不空
  const known = localFonts.some((f) => `local:${f}` === value);
  const locals = React.useMemo(
    () => (isLocal && !known ? [value.slice("local:".length), ...localFonts] : localFonts),
    [localFonts, isLocal, known, value]
  );

  return (
    <div className="flex flex-col gap-1.5">
      <Select
        value={customOpen ? "__custom__" : value}
        onValueChange={(v) => {
          if (v === "__custom__") {
            setCustomOpen(true);
            return;
          }
          setCustomOpen(false);
          onChange(v);
        }}
      >
        <SelectTrigger className="h-8 w-52 text-xs">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {presets.map((f) => (
            <SelectItem key={f.id} value={f.id}>
              {f.label}
            </SelectItem>
          ))}
          {locals.length > 0 && (
            <SelectGroup>
              <SelectSeparator />
              <SelectLabel>{t("appearance.localFonts")}</SelectLabel>
              {locals.slice(0, 400).map((f) => (
                <SelectItem key={f} value={`local:${f}`}>
                  {f}
                </SelectItem>
              ))}
            </SelectGroup>
          )}
          <SelectSeparator />
          <SelectItem value="__custom__">{customLabel}</SelectItem>
        </SelectContent>
      </Select>
      {(customOpen || isLocal) && (
        <Input
          autoFocus={customOpen}
          defaultValue={isLocal ? value.slice("local:".length) : ""}
          placeholder={customLabel}
          className="h-7 w-52 font-mono text-[11px]"
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
          }}
          onBlur={(e) => {
            const fam = e.target.value.trim().replace(/"/g, "");
            if (fam) onChange(`local:${fam}`);
          }}
        />
      )}
    </div>
  );
}

export default function SettingsPage() {
  const t = useT();
  const { setTheme, resolvedTheme } = useTheme();
  const setLang = useUI((s) => s.setLang);
  const setCodeDefaults = useUI((s) => s.setCodeDefaults);
  const [settings, setSettings] = React.useState<AppSettings | null>(null);
  const [checking, setChecking] = React.useState(false);
  const [dataDir, setDataDir] = React.useState("");
  const [appVersion, setAppVersion] = React.useState("");
  const [dragging, setDragging] = React.useState(false);
  const [updateOpen, setUpdateOpen] = React.useState(false);
  const [active, setActive] = React.useState<string>("appearance");
  const [importConfirm, setImportConfirm] = React.useState<string | null>(null);
  const [localFonts, setLocalFonts] = React.useState<string[]>([]);
  const { data: stacks } = useStacks();

  const load = React.useCallback(async () => {
    try {
      const s = await api.getSettings();
      setSettings(s);
      setLang(s.language);
      // 外观统一走 applyAppearance，避免只生效一半（主题色变了字体没变这类）
      applyAppearance(s);
      setCodeDefaults({ lineNumbers: s.codeLineNumbers, wrap: s.codeWrap, theme: s.codeTheme, bg: s.codeBg });
    } catch (e) {
      toastError(e);
    }
  }, [setLang, setCodeDefaults]);

  React.useEffect(() => {
    load();
    api.getAppVersion().then(setAppVersion).catch(() => undefined);
    api.getDataDir().then(setDataDir).catch(() => setDataDir(""));
  }, [load]);

  const update = async (key: keyof AppSettings, value: unknown) => {
    if (!settings) return;
    const next = { ...settings, [key]: value } as AppSettings;
    setSettings(next);
    applyAppearance(next);
    if (
      key === "codeLineNumbers" ||
      key === "codeWrap" ||
      key === "codeTheme" ||
      key === "codeBg"
    ) {
      setCodeDefaults({
        lineNumbers: next.codeLineNumbers,
        wrap: next.codeWrap,
        theme: next.codeTheme,
        bg: next.codeBg,
      });
    }
    try {
      await api.setSetting(key, value);
    } catch (e) {
      toastError(e);
    }
  };

  /** 本机字体：进设置页先做零权限 canvas 探测（不用点按钮就有列表）；
      「扫描本机字体」再做原生枚举 + 探测合并。两条路都拿不到才提示手填。 */
  const loadLocalFonts = React.useCallback(() => {
    setLocalFonts(detectLocalFonts());
  }, []);

  React.useEffect(() => {
    loadLocalFonts();
  }, [loadLocalFonts]);

  const scanLocalFonts = React.useCallback(async () => {
    try {
      const { families, native } = await scanFonts();
      if (families.length === 0) throw new Error("empty");
      setLocalFonts(families);
      toast.success(t("appearance.scanDone").replace("{n}", String(families.length)), {
        description: native ? undefined : t("appearance.scanCanvasHint"),
      });
    } catch {
      toast.error(t("appearance.scanFail"), { description: t("appearance.scanFailHint") });
    }
  }, [t]);

  /** 导入成功后的统一收尾：重载设置 + 通知其它页面刷新缓存 */
  const afterImport = React.useCallback(
    (r: api.ImportReport) => {
      const desc = [
        `${t("settings.backup.rptSites")}: ${r.sites}`,
        `${t("settings.backup.rptSettings")}: ${r.settings}`,
        `${t("settings.backup.rptProfiles")}: ${r.proxyProfiles}`,
        `${t("settings.backup.rptStacks")}: ${r.stacks}`,
        ...(r.certAutomations > 0 ? [`${t("settings.backup.rptCertAuto")}: ${r.certAutomations}`] : []),
        ...(r.certMonitors > 0 ? [`${t("settings.backup.rptCertMon")}: ${r.certMonitors}`] : []),
        ...(r.skippedSites > 0 ? [`${t("settings.backup.rptSkipped")}: ${r.skippedSites}`] : []),
      ].join(" · ");
      if (r.missingPackages.length > 0) {
        toast.warning(t("settings.backup.importDone"), {
          description: `${desc}\n${t("settings.backup.missing")}: ${r.missingPackages.join(", ")}`,
          duration: 10000,
        });
      } else {
        toast.success(t("settings.backup.importDone"), { description: desc });
      }
      load();
      // 端口/栈/站点都可能变了，让挂着这些查询的页面重新拉一次
      window.dispatchEvent(new CustomEvent("nsb:config-imported"));
    },
    [t, load]
  );

  /**
   * 拖拽导入。Tauri 关掉了窗口级原生拖放（dragDropEnabled=false），
   * 但 DOM 上的 HTML5 文件拖放照常工作——这条路在浏览器与桌面端一致，
   * 所以两种环境都启用。
   */
  const onDrop = React.useCallback(
    async (e: React.DragEvent) => {
      e.preventDefault();
      setDragging(false);
      const file = e.dataTransfer.files?.[0];
      if (!file) return;
      if (!file.name.toLowerCase().endsWith(".json")) {
        toast.error(t("settings.backup.dropWrongType"));
        return;
      }
      try {
        // WebView 拿不到拖进来文件的真实路径 → 读文本交给后端解析
        const text = await file.text();
        const r = await api.importConfigText(text);
        afterImport(r);
      } catch (err) {
        toastError(err, t("settings.backup.dropFailed"));
      }
    },
    [t, afterImport]
  );

  if (!settings) {
    return (
      <div className="flex h-64 items-center justify-center text-sm text-faint">{t("common.loading")}</div>
    );
  }

  const profile: "safe" | "standard" = settings.portProfile === "safe" ? "safe" : "standard";
  const overrides = settings.portOverrides ?? {};

  const setPort = async (key: string, value: number | null) => {
    if (value !== null && (value < 1 || value > 65535 || !Number.isInteger(value))) {
      toast.error(t("settings.ports.invalid"));
      return;
    }
    const next = { ...overrides };
    if (value === null) delete next[key];
    else next[key] = value;
    setSettings({ ...settings, portOverrides: next });
    try {
      await api.setPortOverride(key, value);
      toast.success(value === null ? t("settings.ports.resetDone") : t("settings.ports.saved"));
    } catch (e) {
      toastError(e);
    }
  };

  const resetAppearance = async () => {
    const defaults: Partial<AppSettings> = {
      accentHue: 211,
      accentHex: "",
      uiFont: "sf",
      uiScale: 1,
      codeFont: "sf-mono",
      codeFontSize: 11.5,
      codeLineNumbers: true,
      codeWrap: true,
      codeTheme: "auto",
      codeBg: "",
      hideScrollbars: true,
    };
    const next = { ...settings, ...defaults } as AppSettings;
    setSettings(next);
    applyAppearance(next);
    setCodeDefaults({ lineNumbers: true, wrap: true, theme: "auto", bg: "" });
    for (const [k, v] of Object.entries(defaults)) {
      await api.setSetting(k, v).catch(() => undefined);
    }
    toast.success(t("appearance.resetDone"));
  };

  return (
    <div
      className="relative pb-8"
      onDragOver={(e) => {
        // 只有拖文件才亮投放区，避免拖文本/链接误触发
        if (!e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget.contains(e.relatedTarget as Node)) return;
        setDragging(false);
      }}
      onDrop={onDrop}
    >
      {dragging && (
        <div className="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-background/70 backdrop-blur-sm">
          <div className="flex flex-col items-center gap-3 rounded-2xl border-2 border-dashed border-primary/60 bg-card px-12 py-10">
            <FileJson className="h-10 w-10 text-primary" strokeWidth={1.4} />
            <p className="text-sm font-medium">{t("settings.backup.dropHere")}</p>
            <p className="text-[11.5px] text-faint">{t("settings.backup.dropHint")}</p>
          </div>
        </div>
      )}

      <PageHeader title={t("settings.title")} subtitle={t("settings.subtitle")} />

      {/* 窄窗口：左侧竖排导航放不下，改为横向一排 —— 不然除「外观」外的分区根本进不去 */}
      <div className="mb-4 flex gap-1 overflow-x-auto pb-1 lg:hidden">
        {SECTIONS.map((s) => (
          <button
            key={s.id}
            type="button"
            onClick={() => setActive(s.id)}
            className={cn(
              "flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-[12px] transition-colors",
              active === s.id
                ? "border-primary/40 bg-primary-soft font-medium text-primary"
                : "border-border bg-card-2/40 text-muted hover:text-foreground"
            )}
          >
            <s.icon className="h-3.5 w-3.5" />
            {t(s.labelKey as never)}
          </button>
        ))}
      </div>

      <div className="flex gap-6">
        {/* 左侧分区导航：设置项一多，卡片平铺就难找了 */}
        <nav className="sticky top-0 hidden h-fit w-[172px] shrink-0 flex-col gap-0.5 lg:flex">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setActive(s.id)}
              className={cn(
                "flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[12.5px] transition-colors",
                active === s.id
                  ? "bg-card-2 font-medium text-foreground"
                  : "text-muted hover:bg-fill hover:text-foreground"
              )}
            >
              <s.icon className="h-3.5 w-3.5 shrink-0" />
              {t(s.labelKey as never)}
            </button>
          ))}
        </nav>

        <div className="flex min-w-0 flex-1 flex-col gap-5">
          {/* ==================== 外观 ==================== */}
          {active === "appearance" && (
            <>
              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <Palette className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("settings.appearance")}</CardTitle>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="ml-auto h-7 text-[11px] text-faint hover:text-foreground"
                    onClick={resetAppearance}
                  >
                    <RotateCcw className="h-3 w-3" /> {t("appearance.reset")}
                  </Button>
                </CardHeader>
                <CardContent className="flex flex-col gap-5">
                  <SettingRow label={t("settings.theme")}>
                    <div className="flex gap-1 rounded-lg bg-card-2/60 p-1">
                      {(
                        [
                          { v: "dark", icon: Moon, label: t("settings.appearance.dark") },
                          { v: "light", icon: Sun, label: t("settings.appearance.light") },
                          { v: "system", icon: Monitor, label: t("settings.appearance.system") },
                        ] as const
                      ).map((opt) => (
                        <button
                          key={opt.v}
                          onClick={() => {
                            setTheme(opt.v);
                            update("appearance", opt.v);
                          }}
                          className={cn(
                            "flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[11.5px] font-medium transition-all",
                            settings.appearance === opt.v
                              ? "bg-surface text-foreground shadow-sm"
                              : "text-faint hover:text-secondary"
                          )}
                        >
                          <opt.icon className="h-3 w-3" />
                          {opt.label}
                        </button>
                      ))}
                    </div>
                  </SettingRow>

                  {/* 主题色：预设色卡 + 自定义取色器 */}
                  <div className="flex flex-col gap-3">
                    <div className="flex items-center justify-between">
                      <span className="text-[12.5px] text-secondary">{t("settings.accent")}</span>
                      <span className="font-mono text-[10.5px] text-faint">
                        {settings.accentHex || `hue ${effectiveHue(settings)}`}
                      </span>
                    </div>
                    <div className="flex flex-wrap items-center gap-2">
                      {ACCENT_PRESETS.map((p) => {
                        const on = matchingPreset(settings) === p.id;
                        return (
                          <button
                            key={p.id}
                            type="button"
                            title={p.label}
                            onClick={() => {
                              const next = { ...settings, accentHue: p.hue, accentHex: "" };
                              setSettings(next);
                              applyAppearance(next);
                              api.setSetting("accentHue", p.hue).catch(toastError);
                              api.setSetting("accentHex", "").catch(toastError);
                            }}
                            className={cn(
                              "group/sw relative flex h-9 w-9 items-center justify-center rounded-full border transition-all",
                              on ? "border-foreground/30 ring-2 ring-primary/40" : "border-border hover:scale-105"
                            )}
                            style={{ background: `hsl(${p.hue} 82% ${resolvedTheme === "dark" ? 62 : 55}%)` }}
                          >
                            {on && <Check className="h-3.5 w-3.5 text-white drop-shadow" />}
                          </button>
                        );
                      })}
                      {/* 自定义：原生取色器 + 十六进制输入 */}
                      <div
                        className={cn(
                          "flex items-center gap-1.5 rounded-full border px-2 py-1",
                          settings.accentHex ? "border-primary/40 bg-primary-soft" : "border-dashed border-border"
                        )}
                      >
                        <label
                          className="relative h-6 w-6 cursor-pointer overflow-hidden rounded-full border border-border"
                          style={{
                            background:
                              settings.accentHex ||
                              `hsl(${settings.accentHue} 82% ${resolvedTheme === "dark" ? 62 : 55}%)`,
                          }}
                        >
                          <input
                            type="color"
                            value={settings.accentHex || hslToHex(settings.accentHue, 82, 55)}
                            onChange={(e) => {
                              const hex = e.target.value;
                              const next = { ...settings, accentHex: hex };
                              setSettings(next);
                              applyAppearance(next);
                            }}
                            onBlur={(e) => update("accentHex", e.target.value)}
                            className="absolute inset-0 cursor-pointer opacity-0"
                          />
                        </label>
                        <span className="text-[11px] text-faint">{t("appearance.accentCustom")}</span>
                        <HexInput
                          value={settings.accentHex}
                          onCommit={(hex) => update("accentHex", hex)}
                          invalidLabel={t("appearance.accentInvalid")}
                        />
                      </div>
                    </div>
                    <p className="text-[10.5px] text-faint">{t("appearance.accentCustomHint")}</p>
                  </div>

                  <Divider />

                  {/* 字体与字号 */}
                  <div className="flex flex-col gap-3">
                    <div className="flex items-center gap-2">
                      <Type className="h-3.5 w-3.5 text-faint" />
                      <span className="text-[12.5px] font-medium text-secondary">{t("appearance.fonts")}</span>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="ml-auto h-6 text-[11px] text-faint hover:text-foreground"
                        onClick={() => void scanLocalFonts()}
                      >
                        <RefreshCw className="h-3 w-3" /> {t("appearance.scanFonts")}
                      </Button>
                    </div>
                    <SettingRow label={t("appearance.uiFont")}>
                      <FontSelect
                        value={settings.uiFont}
                        presets={UI_FONT_OPTIONS}
                        localFonts={localFonts}
                        onChange={(v) => update("uiFont", v)}
                        customLabel={t("appearance.customFont")}
                      />
                    </SettingRow>
                    <SettingRow label={t("appearance.uiScale")}>
                      <Select value={String(settings.uiScale)} onValueChange={(v) => update("uiScale", Number(v))}>
                        <SelectTrigger className="h-8 w-52 text-xs">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {UI_SCALES.map((s) => (
                            <SelectItem key={s} value={String(s)}>
                              {Math.round(s * 100)}%
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </SettingRow>
                    <SettingRow label={t("appearance.codeFont")}>
                      <FontSelect
                        value={settings.codeFont}
                        presets={MONO_FONT_OPTIONS}
                        localFonts={localFonts}
                        onChange={(v) => update("codeFont", v)}
                        customLabel={t("appearance.customFont")}
                      />
                    </SettingRow>
                    <SettingRow label={t("appearance.codeFontSize")}>
                      <Select
                        value={String(settings.codeFontSize)}
                        onValueChange={(v) => update("codeFontSize", Number(v))}
                      >
                        <SelectTrigger className="h-8 w-52 text-xs">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {CODE_SIZES.map((s) => (
                            <SelectItem key={s} value={String(s)}>
                              {s} px
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </SettingRow>
                  </div>

                  {/* 实时预览 */}
                  <div className="overflow-hidden rounded-xl border border-border">
                    <div className="border-b border-border bg-card-2/30 px-3 py-1.5 text-[10.5px] font-medium uppercase tracking-wide text-faint">
                      {t("appearance.preview")}
                    </div>
                    <div className="flex flex-col gap-3 p-4">
                      <p className="text-[15px] font-semibold tracking-tight">{t("appearance.previewText")}</p>
                      <div className="flex flex-wrap items-center gap-2">
                        <Button size="sm">{t("common.confirm")}</Button>
                        <Button size="sm" variant="secondary">
                          {t("common.cancel")}
                        </Button>
                        <span className="rounded-full bg-primary px-2 py-0.5 text-[10px] font-medium text-primary-fg">
                          {t("update.badge")}
                        </span>
                        <Badge variant="running">{t("common.running")}</Badge>
                      </div>
                      {/* 实时预览走真正的 CodeBlock：代码主题/背景/字体/换行/行号改了立刻能看到 */}
                      <CodeBlock
                        compact
                        lang="nginx"
                        code={`# 预览：站点配置长这样\nserver {\n    listen ${profile === "safe" ? 8080 : 80};\n    server_name demo.${settings.defaultTld};\n    root "/www/demo";\n}`}
                      />
                      <p className="text-[11px] text-faint">{t("appearance.previewHint")}</p>
                    </div>
                  </div>
                </CardContent>
              </Card>

              {/* 显示与代码块默认形态 */}
              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <StretchHorizontal className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("appearance.display")}</CardTitle>
                </CardHeader>
                <CardContent className="flex flex-col gap-4">
                  <ToggleRow
                    label={t("appearance.hideScrollbars")}
                    hint={t("appearance.hideScrollbarsHint")}
                    checked={settings.hideScrollbars}
                    onChange={(v) => update("hideScrollbars", v)}
                  />
                  <ToggleRow
                    label={t("settings.reduceMotion")}
                    checked={settings.reduceMotion}
                    onChange={(v) => update("reduceMotion", v)}
                  />
                  <Divider />
                  <div className="flex items-center gap-2">
                    <FileJson className="h-3.5 w-3.5 text-faint" />
                    <span className="text-[12.5px] font-medium text-secondary">
                      {t("appearance.codeDefaults")}
                    </span>
                  </div>
                  <SettingRow label={t("appearance.codeTheme")}>
                    <Select value={settings.codeTheme} onValueChange={(v) => update("codeTheme", v)}>
                      <SelectTrigger className="h-8 w-52 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {CODE_THEME_OPTIONS.map((th) => (
                          <SelectItem key={th.id} value={th.id}>
                            {th.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </SettingRow>
                  <SettingRow label={t("appearance.codeBg")}>
                    <div className="flex items-center gap-2">
                      <label
                        className="relative h-6 w-6 cursor-pointer overflow-hidden rounded-md border border-border"
                        style={{ background: settings.codeBg || "var(--card-2)" }}
                        title={t("appearance.codeBg")}
                      >
                        <input
                          type="color"
                          value={settings.codeBg || "#0a0c0f"}
                          onChange={(e) => setSettings({ ...settings, codeBg: e.target.value })}
                          onBlur={(e) => update("codeBg", e.target.value)}
                          className="absolute inset-0 cursor-pointer opacity-0"
                        />
                      </label>
                      <HexInput
                        value={settings.codeBg}
                        onCommit={(hex) => update("codeBg", hex)}
                        invalidLabel={t("appearance.accentInvalid")}
                      />
                      {settings.codeBg && (
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-6 text-[11px] text-faint hover:text-foreground"
                          onClick={() => update("codeBg", "")}
                        >
                          <RotateCcw className="h-3 w-3" /> {t("appearance.codeBgReset")}
                        </Button>
                      )}
                    </div>
                  </SettingRow>
                  <p className="text-[10.5px] text-faint">{t("appearance.codeThemeHint")}</p>
                  <ToggleRow
                    label={t("code.lineNumbers")}
                    checked={settings.codeLineNumbers}
                    onChange={(v) => update("codeLineNumbers", v)}
                  />
                  <ToggleRow
                    label={t("code.wrap")}
                    checked={settings.codeWrap}
                    onChange={(v) => update("codeWrap", v)}
                  />
                </CardContent>
              </Card>
            </>
          )}

          {/* ==================== 通用 ==================== */}
          {active === "general" && (
            <Card>
              <CardHeader className="flex-row items-center gap-3">
                <Settings2 className="h-4 w-4 text-primary" />
                <CardTitle className="text-[13px]">{t("settings.general")}</CardTitle>
              </CardHeader>
              <CardContent className="flex flex-col gap-4">
                <SettingRow label={t("settings.tld")}>
                  <Input
                    value={settings.defaultTld}
                    onChange={(e) => setSettings({ ...settings, defaultTld: e.target.value })}
                    onBlur={(e) => update("defaultTld", e.target.value)}
                    className="h-8 w-28 font-mono text-xs"
                  />
                </SettingRow>
                <SettingRow label={t("settings.webServer")}>
                  <Select value={settings.defaultWebServer} onValueChange={(v) => update("defaultWebServer", v)}>
                    <SelectTrigger className="h-8 w-32 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="nginx">Nginx</SelectItem>
                      <SelectItem value="apache">Apache</SelectItem>
                    </SelectContent>
                  </Select>
                </SettingRow>
                <SettingRow label={t("settings.language")}>
                  <Select
                    value={settings.language}
                    onValueChange={(v) => {
                      update("language", v);
                      setLang(v as "zh" | "en");
                    }}
                  >
                    <SelectTrigger className="h-8 w-32 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="zh">简体中文</SelectItem>
                      <SelectItem value="en">English</SelectItem>
                    </SelectContent>
                  </Select>
                </SettingRow>
                <Divider />
                <SettingRow label={t("settings.autostart")}>
                  <Switch checked={settings.autostart} onCheckedChange={(v) => update("autostart", v)} />
                </SettingRow>
                <SettingRow label={t("settings.minimizeToTray")}>
                  <Switch checked={settings.minimizeToTray} onCheckedChange={(v) => update("minimizeToTray", v)} />
                </SettingRow>
                <SettingRow label={t("settings.startStack")}>
                  <Select
                    value={settings.startStackOnLaunch || "__none__"}
                    onValueChange={(v) => update("startStackOnLaunch", v === "__none__" ? "" : v)}
                  >
                    <SelectTrigger className="h-8 w-52 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="__none__">{t("settings.startStackNone")}</SelectItem>
                      {stacks.map((s) => (
                        <SelectItem key={s.id} value={s.id}>
                          {s.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </SettingRow>
              </CardContent>
            </Card>
          )}

          {/* ==================== 服务与端口 ==================== */}
          {active === "services" && (
            <>
              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <Network className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("settings.ports")}</CardTitle>
                </CardHeader>
                <CardContent className="flex flex-col gap-4">
                  <SettingRow label={t("settings.ports")}>
                    <Select value={settings.portProfile} onValueChange={(v) => update("portProfile", v)}>
                      <SelectTrigger className="h-8 w-72 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="standard">{t("settings.ports.standard")}</SelectItem>
                        <SelectItem value="safe">{t("settings.ports.safe")}</SelectItem>
                      </SelectContent>
                    </Select>
                  </SettingRow>
                  <p className="text-[11.5px] text-faint">{t("settings.ports.hint")}</p>

                  <div className="overflow-hidden rounded-xl border border-border">
                    <div className="flex items-center justify-between border-b border-border bg-card-2/25 px-3 py-2">
                      <span className="text-[12px] font-medium text-secondary">
                        {t("settings.ports.individual")}
                      </span>
                      <span className="text-[10.5px] text-faint">{t("settings.ports.default")}</span>
                    </div>
                    <div className="grid grid-cols-1 sm:grid-cols-2">
                      {PORT_FIELDS.map((f, idx) => {
                        const custom = overrides[f.key];
                        const fallback = f.fallback[profile];
                        return (
                          <div
                            key={f.key}
                            className={cn(
                              "flex items-center justify-between gap-2 px-3 py-2",
                              idx < PORT_FIELDS.length - (PORT_FIELDS.length % 2 === 0 ? 2 : 1) &&
                                "border-b border-border",
                              idx % 2 === 0 && "sm:border-r sm:border-border"
                            )}
                          >
                            <div className="flex min-w-0 items-center gap-2">
                              <span className="truncate text-[12px] text-secondary">{f.label}</span>
                              {custom != null && <Badge variant="default">{t("settings.ports.override")}</Badge>}
                            </div>
                            <div className="flex shrink-0 items-center gap-1.5">
                              <Input
                                type="number"
                                min={1}
                                max={65535}
                                value={custom ?? fallback}
                                onChange={(e) => {
                                  const raw = e.target.value;
                                  const next = { ...overrides };
                                  if (!raw) delete next[f.key];
                                  else next[f.key] = Number(raw);
                                  setSettings({ ...settings, portOverrides: next });
                                }}
                                onBlur={(e) => {
                                  const raw = e.target.value.trim();
                                  if (!raw) {
                                    if (custom != null) setPort(f.key, null);
                                    return;
                                  }
                                  const n = Number(raw);
                                  // 改回档位默认值 = 取消覆盖
                                  if (n === fallback) {
                                    if (custom != null) setPort(f.key, null);
                                  } else {
                                    setPort(f.key, n);
                                  }
                                }}
                                className={cn(
                                  "h-7 w-24 font-mono text-[11.5px]",
                                  custom != null && "border-primary/40 text-foreground"
                                )}
                              />
                              {custom != null && (
                                <Button
                                  size="icon-sm"
                                  variant="ghost"
                                  title={t("settings.ports.reset")}
                                  className="text-faint hover:text-foreground"
                                  onClick={() => setPort(f.key, null)}
                                >
                                  <RefreshCw className="h-3 w-3" />
                                </Button>
                              )}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                </CardContent>
              </Card>

              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <Rocket className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("settings.startup")}</CardTitle>
                </CardHeader>
                <CardContent className="flex flex-col gap-4">
                  <ToggleRow
                    label={
                      <span className="flex items-center gap-2">
                        <ShieldAlert className="h-3.5 w-3.5 text-faint" />
                        {t("settings.autoClosePort")}
                      </span>
                    }
                    hint={t("settings.autoClosePortHint")}
                    checked={settings.autoClosePortOnStart}
                    onChange={(v) => update("autoClosePortOnStart", v)}
                  />
                  <ToggleRow
                    label={
                      <span className="flex items-center gap-2">
                        <Activity className="h-3.5 w-3.5 text-faint" />
                        {t("settings.watchdog")}
                      </span>
                    }
                    hint={t("settings.watchdogHint")}
                    checked={settings.watchdogEnabled === true}
                    onChange={(v) => update("watchdogEnabled", v)}
                  />
                </CardContent>
              </Card>
            </>
          )}

          {/* ==================== 日志与交互 ==================== */}
          {active === "logs" && (
            <Card>
              <CardHeader className="flex-row items-center gap-3">
                <ScrollText className="h-4 w-4 text-primary" />
                <CardTitle className="text-[13px]">{t("settings.logs")}</CardTitle>
              </CardHeader>
              <CardContent className="flex flex-col gap-4">
                <SettingRow label={t("settings.logTailLines")}>
                  <Select
                    value={String(settings.logTailLines)}
                    onValueChange={(v) => update("logTailLines", Number(v))}
                  >
                    <SelectTrigger className="h-8 w-32 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {[200, 500, 1000, 2000].map((n) => (
                        <SelectItem key={n} value={String(n)}>
                          {n}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </SettingRow>
                <SettingRow label={t("settings.logAutoRefresh")}>
                  <Switch checked={settings.logAutoRefresh} onCheckedChange={(v) => update("logAutoRefresh", v)} />
                </SettingRow>
                <ToggleRow
                  label={t("settings.confirmKill")}
                  hint={t("settings.confirmKillHint")}
                  checked={settings.confirmKill}
                  onChange={(v) => update("confirmKill", v)}
                />
              </CardContent>
            </Card>
          )}

          {/* ==================== 数据与备份 ==================== */}
          {active === "storage" && (
            <>
              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <FolderTree className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("settings.dataDir")}</CardTitle>
                </CardHeader>
                <CardContent className="flex flex-col gap-3">
                  <p className="text-[11.5px] text-faint">{t("settings.dataDirHint")}</p>
                  <div className="flex items-center gap-2">
                    <code className="flex-1 truncate rounded-lg bg-card-2/50 px-3 py-2 font-mono text-[12px] text-secondary">
                      {dataDir || t("settings.dataDirUnknown")}
                    </code>
                    <CopyButton text={dataDir} />
                    <Button variant="secondary" onClick={() => api.openInFolder(dataDir || ".").catch(toastError)}>
                      {t("common.open")}
                    </Button>
                    <Button
                      variant="outline"
                      onClick={() =>
                        toast.info(t("settings.migrateToastTitle"), {
                          description: t("settings.migrateToastDesc"),
                        })
                      }
                    >
                      {t("settings.migrate")}
                    </Button>
                  </div>
                </CardContent>
              </Card>

              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <Download className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("settings.mirror")}</CardTitle>
                </CardHeader>
                <CardContent className="flex flex-col gap-4">
                  <SettingRow label={t("settings.mirror")}>
                    <Select value={settings.mirror} onValueChange={(v) => update("mirror", v)}>
                      <SelectTrigger className="h-8 w-56 text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="official">{t("settings.mirror.official")}</SelectItem>
                        <SelectItem value="ghproxy">{t("settings.mirror.ghproxy")}</SelectItem>
                        <SelectItem value="custom">{t("settings.mirror.custom")}</SelectItem>
                      </SelectContent>
                    </Select>
                  </SettingRow>

                  {/* 工具链镜像：与套件下载镜像分开，因为它影响的是用户项目里的包管理器 */}
                  <div className="mt-4 border-t border-border pt-4">
                    <ToolMirrorCard />
                  </div>
                  {settings.mirror === "custom" && (
                    <Input
                      value={settings.customMirror}
                      onChange={(e) => setSettings({ ...settings, customMirror: e.target.value })}
                      onBlur={(e) => update("customMirror", e.target.value)}
                      placeholder="https://mirror.example.com/"
                      className="font-mono text-xs"
                    />
                  )}
                </CardContent>
              </Card>

              {/* 配置导入/导出（支持拖拽） */}
              <Card>
                <CardHeader className="flex-row items-center gap-3">
                  <Archive className="h-4 w-4 text-primary" />
                  <CardTitle className="text-[13px]">{t("settings.backup.title")}</CardTitle>
                </CardHeader>
                <CardContent className="flex flex-col gap-3">
                  <p className="text-[11.5px] text-faint">{t("settings.backup.hint")}</p>

                  {/* 常驻投放区：提示「这里可以拖」 */}
                  <div
                    className={cn(
                      "flex items-center gap-3 rounded-xl border-2 border-dashed px-4 py-3 transition-colors",
                      dragging ? "border-primary/70 bg-primary-soft" : "border-border bg-card-2/20"
                    )}
                  >
                    <FileJson className="h-5 w-5 shrink-0 text-faint" />
                    <div className="min-w-0 flex-1">
                      <p className="text-[12px] font-medium text-secondary">{t("settings.backup.dropHere")}</p>
                      <p className="text-[10.5px] text-faint">{t("settings.backup.dropHint")}</p>
                    </div>
                  </div>

                  <div className="flex flex-wrap items-center gap-2">
                    <Button
                      variant="secondary"
                      disabled={!isTauri}
                      onClick={async () => {
                        try {
                          const { save } = await import("@tauri-apps/plugin-dialog");
                          const now = new Date();
                          const stamp = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, "0")}${String(now.getDate()).padStart(2, "0")}`;
                          const path = await save({
                            title: t("settings.backup.exportTitle"),
                            defaultPath: `nsb-backup-${stamp}.json`,
                            filters: [{ name: "NiceEnv Backup", extensions: ["json"] }],
                          });
                          if (!path) return;
                          const n = await api.exportConfig(path);
                          toast.success(t("settings.backup.exportDone"), {
                            description: `${n} ${t("settings.backup.items")}`,
                          });
                        } catch (e) {
                          toastError(e);
                        }
                      }}
                    >
                      <HardDriveDownload className="h-3.5 w-3.5" /> {t("settings.backup.export")}
                    </Button>
                    <Button
                      variant="secondary"
                      disabled={!isTauri}
                      onClick={async () => {
                        try {
                          const { open } = await import("@tauri-apps/plugin-dialog");
                          const path = await open({
                            title: t("settings.backup.importTitle"),
                            multiple: false,
                            filters: [{ name: "NiceEnv Backup", extensions: ["json"] }],
                          });
                          if (!path || typeof path !== "string") return;
                          setImportConfirm(path);
                        } catch (e) {
                          toastError(e);
                        }
                      }}
                    >
                      <Upload className="h-3.5 w-3.5" /> {t("settings.backup.import")}
                    </Button>
                  </div>
                </CardContent>
              </Card>
            </>
          )}

          {/* ==================== 更新 ==================== */}
          {active === "updates" && (
            <Card>
              <CardHeader className="flex-row items-center gap-3">
                <RefreshCw className="h-4 w-4 text-primary" />
                <CardTitle className="text-[13px]">{t("settings.updates")}</CardTitle>
              </CardHeader>
              <CardContent className="flex flex-col gap-4">
                <div className="flex items-center justify-between gap-4 rounded-xl bg-fill p-3.5">
                  <div className="flex items-center gap-3">
                    <div className="flex h-10 w-10 items-center justify-center rounded-xl border border-border bg-card">
                      <Globe className="h-4 w-4 text-primary" strokeWidth={1.8} />
                    </div>
                    <div className="flex flex-col">
                      <span className="text-[12.5px] text-secondary">
                        {t("settings.currentVersion")}{" "}
                        <code className="font-mono text-foreground">v{appVersion || "0.2.6"}</code>
                      </span>
                      <span className="text-[10.5px] text-faint">{t("settings.manifestHint")}</span>
                    </div>
                  </div>
                  <Button
                    disabled={checking}
                    onClick={() => {
                      setUpdateOpen(true);
                    }}
                  >
                    <RefreshCw className={cn("h-3.5 w-3.5", checking && "animate-spin")} />
                    {t("settings.checkUpdate")}
                  </Button>
                </div>

                <ToggleRow
                  label={t("update.checkOnLaunch")}
                  hint={t("update.checkOnLaunchHint")}
                  checked={settings.checkUpdateOnLaunch}
                  onChange={(v) => update("checkUpdateOnLaunch", v)}
                />
                <ToggleRow
                  label={t("update.autoDownload")}
                  hint={t("update.autoDownloadHint")}
                  checked={settings.autoDownloadUpdate}
                  onChange={(v) => update("autoDownloadUpdate", v)}
                />

                <Divider />

                <SettingRow label={t("settings.manifestUrl")}>
                  <Input
                    value={settings.manifestUrl}
                    onChange={(e) => setSettings({ ...settings, manifestUrl: e.target.value })}
                    onBlur={(e) => update("manifestUrl", e.target.value)}
                    placeholder="https://…/packages.win.json"
                    className="h-8 w-80 font-mono text-[11px]"
                  />
                </SettingRow>
                <div className="flex items-center gap-2">
                  <p className="text-[10.5px] text-faint">{t("settings.manifestUrlHint")}</p>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="ml-auto h-7 shrink-0 text-[11px] text-faint hover:text-foreground"
                    onClick={() =>
                      api
                        .resetRemoteManifest()
                        .then(() => toast.success(t("settings.resetManifestDone")))
                        .catch(toastError)
                    }
                  >
                    <RotateCcw className="h-3 w-3" /> {t("settings.resetManifest")}
                  </Button>
                </div>
              </CardContent>
            </Card>
          )}

          {/* ==================== 高级 ==================== */}
          {active === "advanced" && (
            <Card>
              <CardHeader className="flex-row items-center gap-3">
                <SlidersHorizontal className="h-4 w-4 text-primary" />
                <CardTitle className="text-[13px]">{t("settings.section.advanced")}</CardTitle>
              </CardHeader>
              <CardContent className="flex flex-col gap-4">
                <p className="text-[11.5px] text-faint">{t("settings.advancedHint")}</p>
                <Divider />
                <div className="flex items-center justify-between gap-4">
                  <div className="flex flex-col gap-0.5">
                    <span className="text-[12.5px] text-secondary">{t("about.desc")}</span>
                    <span className="text-[10.5px] text-faint">
                      {t("settings.currentVersion")} v{appVersion || "0.2.6"}
                    </span>
                  </div>
                  <div className="flex gap-2">
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => api.openInFolder(dataDir || ".").catch(toastError)}
                    >
                      <FolderTree className="h-3.5 w-3.5" /> {t("appmenu.dataDir")}
                    </Button>
                  </div>
                </div>
              </CardContent>
            </Card>
          )}
        </div>
      </div>

      <UpdateDialog open={updateOpen} onOpenChange={setUpdateOpen} />

      <ConfirmDialog
        open={importConfirm !== null}
        onOpenChange={(o) => !o && setImportConfirm(null)}
        title={t("confirm.importConfig")}
        description={t("confirm.importConfigDesc")}
        confirmText={t("settings.backup.import")}
        onConfirm={async () => {
          if (!importConfirm) return;
          try {
            const r = await api.importConfig(importConfirm);
            afterImport(r);
          } catch (e) {
            toastError(e);
          } finally {
            setImportConfirm(null);
          }
        }}
      />
    </div>
  );
}

/* ---------- 小组件 ---------- */

function SettingRow({ label, children }: { label: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <span className="text-[12.5px] text-secondary">{label}</span>
      {children}
    </div>
  );
}

/** 开关行：左边标题（可带说明），右边 Switch —— 比 SettingRow 更能承载解释文案 */
function ToggleRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: React.ReactNode;
  hint?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="flex flex-col gap-0.5">
        <span className="text-[12.5px] text-secondary">{label}</span>
        {hint && <p className="max-w-[42rem] text-[10.5px] leading-snug text-faint">{hint}</p>}
      </div>
      <Switch checked={checked} onCheckedChange={onChange} className="mt-0.5 shrink-0" />
    </div>
  );
}

function Divider() {
  return <div className="h-px bg-border" />;
}

/** 十六进制色值输入：失焦/回车才提交，非法值给提示且不落库 */
function HexInput({
  value,
  onCommit,
  invalidLabel,
}: {
  value: string;
  onCommit: (hex: string) => void;
  invalidLabel: string;
}) {
  const [draft, setDraft] = React.useState(value);
  React.useEffect(() => setDraft(value), [value]);
  const commit = () => {
    const s = draft.trim();
    if (!s) {
      onCommit("");
      return;
    }
    const withHash = s.startsWith("#") ? s : `#${s}`;
    if (!/^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(withHash)) {
      toast.error(invalidLabel);
      setDraft(value);
      return;
    }
    onCommit(withHash.toLowerCase());
  };
  return (
    <input
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
      placeholder="#3b82f6"
      className="h-6 w-[74px] rounded-md border border-border bg-card px-1.5 font-mono text-[11px] outline-none placeholder:text-faint focus:border-primary"
    />
  );
}
