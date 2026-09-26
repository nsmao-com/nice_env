"use client";

import * as React from "react";
import { Check, Copy, Hash, AlignLeft, WrapText, Sparkles } from "lucide-react";
import { useTheme } from "next-themes";
import { cn } from "@/lib/utils";
import { useT } from "@/lib/store";
import { useUI } from "@/lib/store";
import { parseHex } from "@/lib/appearance";
import { CODE_THEME_OPTIONS, type CodePalette } from "@nsb/schema";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

/* ============================================================
   代码块：语法高亮 + 行号 + 一键格式化 + 复制 + 换行开关。
   纯前端实现、零依赖 —— 只覆盖本地环境管理器里真正会出现的
   几种语法（nginx.conf / ini / shell / json / yaml / php / sql / 日志），
   够用且不会为了高亮引入几百 KB 的完整 parser。

   配色：RULES 产出语义 token（注释/关键字/字符串…），具体颜色由
   设置里的代码主题（CODE_THEME_OPTIONS）通过 CSS 变量注入，
   支持跟随界面明暗与自定义背景色。
   ============================================================ */

export type CodeLang =
  | "nginx"
  | "ini"
  | "json"
  | "yaml"
  | "shell"
  | "php"
  | "sql"
  | "env"
  | "markdown"
  | "plain";

const LANG_LABEL: Record<CodeLang, string> = {
  nginx: "nginx",
  ini: "ini",
  json: "json",
  yaml: "yaml",
  shell: "shell",
  php: "php",
  sql: "sql",
  env: "env",
  markdown: "markdown",
  plain: "text",
};

/* ---------- 语法着色 ---------- */

/** 语义 token 种类；颜色见 globals.css 的 .ck-*（由主题变量驱动） */
type TokenKind = "comment" | "key" | "keyword" | "string" | "number" | "variable" | "strong" | "";

interface Rule {
  re: RegExp;
  tk: TokenKind;
}

/**
 * 规则顺序即优先级：注释与字符串必须排在数字/标识符之前，
 * 否则 `# 端口 80` 里的 80 会被当成数字着色、把注释切开。
 */
const RULES: Record<Exclude<CodeLang, "plain">, Rule[]> = {
  nginx: [
    { re: /(^|\s)(#.*)$/m, tk: "comment" },
    { re: /\$[a-zA-Z_]\w*/g, tk: "variable" },
    { re: /\b(server|location|upstream|http|events|map|if|return|rewrite|proxy_pass|fastcgi_pass|listen|server_name|root|index|include|set|add_header|try_files|error_page|ssl_certificate|ssl_certificate_key|worker_processes|worker_connections|keepalive_timeout|client_max_body_size|gzip|expires|deny|allow|alias|proxy_set_header|fastcgi_param)\b/g, tk: "keyword" },
    { re: /\b\d+(\.\d+)?[kKmMgG]?\b/g, tk: "number" },
    { re: /"[^"]*"|'[^']*'/g, tk: "string" },
  ],
  ini: [
    { re: /(^|\s)([;#].*)$/gm, tk: "comment" },
    { re: /^\s*\[[^\]]+\]/gm, tk: "keyword" },
    { re: /^[A-Za-z_][\w.\-]*(?=\s*=)/gm, tk: "key" },
    { re: /\b(on|off|true|false|yes|no|1|0)\b/gi, tk: "number" },
    { re: /"[^"]*"|'[^']*'/g, tk: "string" },
  ],
  json: [
    { re: /"(?:[^"\\]|\\.)*"(?=\s*:)/g, tk: "key" },
    { re: /"(?:[^"\\]|\\.)*"/g, tk: "string" },
    { re: /\b(true|false|null)\b/g, tk: "keyword" },
    { re: /-?\b\d+(\.\d+)?([eE][+-]?\d+)?\b/g, tk: "number" },
  ],
  yaml: [
    { re: /(^|\s)(#.*)$/gm, tk: "comment" },
    { re: /^(\s*)([\w.\-/]+)(?=\s*:)/gm, tk: "key" },
    { re: /(:\s*)("[^"]*"|'[^']*')/g, tk: "string" },
    { re: /\b(true|false|null|yes|no|on|off)\b/gi, tk: "keyword" },
    { re: /\b\d+(\.\d+)?\b/g, tk: "number" },
    { re: /^\s*-\s/gm, tk: "comment" },
  ],
  shell: [
    { re: /(^|\s)(#.*)$/gm, tk: "comment" },
    { re: /\b(PATH|HOME|USER|SHELL|PWD)\b/g, tk: "variable" },
    { re: /\$[\w{}]+/g, tk: "variable" },
    { re: /\b(export|set|source|cd|echo|if|then|else|fi|for|do|done|foreach|function|\$env:|Get-|Set-|Import-|Start-)\b/gi, tk: "keyword" },
    { re: /"[^"]*"|'[^']*'/g, tk: "string" },
    { re: /\b\d+\b/g, tk: "number" },
  ],
  php: [
    { re: /(^|\s)(\/\/.*|\/\*[\s\S]*?\*\/|#.*)$/gm, tk: "comment" },
    { re: /<\?php|<\?=/g, tk: "keyword" },
    { re: /\$[a-zA-Z_]\w*/g, tk: "variable" },
    { re: /\b(function|class|public|private|protected|static|return|new|echo|if|else|foreach|for|while|try|catch|throw|namespace|use|extends|implements|array|fn|match)\b/g, tk: "keyword" },
    { re: /\b(true|false|null|TRUE|FALSE|NULL)\b/g, tk: "number" },
    { re: /"[^"]*"|'[^']*'/g, tk: "string" },
    { re: /\b\d+(\.\d+)?\b/g, tk: "number" },
  ],
  sql: [
    { re: /--[^\n]*/g, tk: "comment" },
    { re: /\b(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP|ALTER|TABLE|DATABASE|USER|GRANT|REVOKE|FROM|WHERE|VALUES|INTO|SET|IDENTIFIED|BY|PRIVILEGES|ON|EXISTS|IF|NOT|DEFAULT|CHARACTER|COLLATE|PRIMARY|KEY|INDEX|LIMIT|ORDER|GROUP)\b/gi, tk: "keyword" },
    { re: /"[^"]*"|'[^']*'|`[^`]*`/g, tk: "string" },
    { re: /\b\d+(\.\d+)?\b/g, tk: "number" },
  ],
  env: [
    { re: /(^|\s)(#.*)$/gm, tk: "comment" },
    { re: /^[A-Z][A-Z0-9_]*(?==)/gm, tk: "key" },
    { re: /=.*$/gm, tk: "string" },
  ],
  markdown: [
    // 标题 → 重一点，正文层次才看得出来
    { re: /^#{1,6} .*$/gm, tk: "strong" },
    { re: /^\s*[-*+] /gm, tk: "keyword" },
    { re: /^\s*> .*$/gm, tk: "comment" },
    { re: /`[^`]*`/g, tk: "string" },
    { re: /^```.*$/gm, tk: "comment" },
    { re: /\[[^\]]*\]\([^)]*\)/g, tk: "variable" },
    { re: /^\|.*\|$/gm, tk: "key" },
    { re: /\*\*[^*]+\*\*/g, tk: "strong" },
  ],
};

/** 把一段代码切成 [{text, tk}] —— 逐行处理，保证行号与内容对应 */
function tokenize(line: string, lang: CodeLang): { text: string; tk: TokenKind }[] {
  if (lang === "plain") return [{ text: line, tk: "" }];
  const rules = RULES[lang];
  // 用一个「占位」数组记录每个字符命中的 token，后写的规则不覆盖先写的（先写优先级高）
  const owner: (TokenKind | null)[] = new Array(line.length).fill(null);
  for (const rule of rules) {
    const re = new RegExp(rule.re.source, rule.re.flags.includes("g") ? rule.re.flags : rule.re.flags + "g");
    let m: RegExpExecArray | null;
    while ((m = re.exec(line)) !== null) {
      if (m[0].length === 0) {
        re.lastIndex += 1;
        continue;
      }
      // 忽略前导空白捕获组：只给真正的内容着色
      const lead = m[1] && m[2] !== undefined ? m[1].length : 0;
      const start = m.index + lead;
      const end = m.index + m[0].length;
      for (let i = start; i < end; i++) {
        if (owner[i] === null) owner[i] = rule.tk;
      }
    }
  }
  const out: { text: string; tk: TokenKind }[] = [];
  let cur = "";
  let curTk: TokenKind | null = null;
  for (let i = 0; i < line.length; i++) {
    const tk = owner[i];
    if (tk === curTk) {
      cur += line[i];
    } else {
      if (cur) out.push({ text: cur, tk: curTk ?? "" });
      cur = line[i];
      curTk = tk;
    }
  }
  if (cur) out.push({ text: cur, tk: curTk ?? "" });
  return out.length > 0 ? out : [{ text: "", tk: "" }];
}

/* ---------- 配色 ---------- */

type ThemeOption = { id: string; label: string; dark?: boolean; colors?: CodePalette };
const THEME_LIST = CODE_THEME_OPTIONS as readonly ThemeOption[];

function themeColors(id: string): CodePalette | undefined {
  return THEME_LIST.find((t) => t.id === id)?.colors;
}

/** #RRGGBB 感知亮度（0–1）：自定义背景时用来挑前景色组 */
function bgLuminance(hex: string): number {
  const p = parseHex(hex);
  if (!p) return 1;
  return (0.2126 * p.r + 0.7152 * p.g + 0.0722 * p.b) / 255;
}

function resolveCodePalette(themeId: string, customBg: string, resolvedDark: boolean): { colors: CodePalette; bg: string } {
  const dark = themeColors("dark")!;
  const light = themeColors("light")!;
  const preset = themeId === "auto" ? undefined : themeColors(themeId);
  let colors = preset ?? (resolvedDark ? dark : light);
  let bg = colors.bg;
  if (customBg) {
    // 背景被用户改掉：按亮度换一套前景/语义色，保证可读
    bg = customBg;
    colors = bgLuminance(customBg) >= 0.5 ? light : dark;
  }
  return { colors, bg };
}

/** 代码、日志和纯文本编辑器共用背景与前景，独立于界面明暗。 */
export function useCodePalette(): React.CSSProperties {
  const { resolvedTheme } = useTheme();
  const settings = useUI((s) => s.codeDefaults);
  const { colors, bg } = resolveCodePalette(settings.theme, settings.bg, resolvedTheme === "dark");
  const dark = bgLuminance(bg) < 0.5;
  return {
    "--code-bg": bg,
    "--code-fg": colors.fg,
    "--code-comment": colors.comment,
    "--code-muted": `color-mix(in srgb, ${colors.fg} 72%, ${bg})`,
    "--code-key": colors.key,
    "--code-keyword": colors.keyword,
    "--code-string": colors.string,
    "--code-number": colors.number,
    "--code-variable": colors.variable,
    "--code-error": dark ? "#FF8589" : "#B4232B",
    "--code-warn": dark ? "#F6BE73" : "#945100",
  } as React.CSSProperties;
}

/* ---------- 格式化 ---------- */

/**
 * 轻量格式化：按语言做「缩进规范化 + 空行收缩 + 行尾去空格」。
 * 不做完整 AST 重排——本地配置文件多半是人手写的，重排反而会丢注释掉语义。
 */
export function formatCode(code: string, lang: CodeLang): string {
  const lines = code.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let blank = 0;
  for (const raw of lines) {
    let line = raw.replace(/[ \t]+$/, "");
    if (lang === "json") {
      line = line.replace(/"(\w+)"\s*:/g, '"$1": ');
    }
    if (line.trim() === "") {
      // 最多保留一个连续空行
      blank += 1;
      if (blank <= 1) out.push("");
      continue;
    }
    blank = 0;
    out.push(line);
  }
  while (out.length > 0 && out[out.length - 1] === "") out.pop();
  return out.join("\n");
}

/** 从语言标签 / 文件名猜语法（用于自动高亮） */
export function guessLang(hint?: string): CodeLang {
  if (!hint) return "plain";
  const h = hint.toLowerCase();
  if (h.includes("php")) return "php";
  if (h.includes("json")) return "json";
  if (h.includes("yaml") || h.includes("yml")) return "yaml";
  if (h.includes("sql") || h.includes("mysql")) return "sql";
  if (h.includes("nginx") || h.includes("conf")) return "nginx";
  if (h.includes("ini") || h.includes("toml")) return "ini";
  if (h.includes("env")) return "env";
  if (h.includes("bash") || h.includes("sh") || h.includes("shell") || h.includes("powershell")) return "shell";
  return "plain";
}

/* ---------- 组件 ---------- */

export function CodeBlock({
  code,
  lang = "plain",
  className,
  maxHeight,
  showLineNumbers,
  wrap,
  title,
  actions,
  compact,
}: {
  code: string;
  lang?: CodeLang;
  className?: string;
  maxHeight?: number | string;
  /** 覆盖设置里的默认值 */
  showLineNumbers?: boolean;
  wrap?: boolean;
  title?: React.ReactNode;
  actions?: React.ReactNode;
  compact?: boolean;
}) {
  const t = useT();
  const paletteVars = useCodePalette();
  const settingsDefaults = useUI((s) => s.codeDefaults);
  const [copied, setCopied] = React.useState(false);
  const [showNum, setShowNum] = React.useState(showLineNumbers ?? settingsDefaults.lineNumbers);
  const [doWrap, setDoWrap] = React.useState(wrap ?? settingsDefaults.wrap);
  const [formatted, setFormatted] = React.useState(false);

  // 设置变化时跟随（用户在本组件里手动切过就不强制覆盖）
  const touched = React.useRef(false);
  React.useEffect(() => {
    if (touched.current) return;
    setShowNum(showLineNumbers ?? settingsDefaults.lineNumbers);
    setDoWrap(wrap ?? settingsDefaults.wrap);
  }, [showLineNumbers, wrap, settingsDefaults.lineNumbers, settingsDefaults.wrap]);

  const active = formatted ? formatCode(code, lang) : code;
  const lines = React.useMemo(() => active.split("\n"), [active]);
  const tokenized = React.useMemo(() => lines.map((l) => tokenize(l, lang)), [lines, lang]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(active);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* 剪贴板不可用：静默 */
    }
  };

  return (
    <div
      className={cn("nsb-code group/code overflow-hidden rounded-xl border border-border", className)}
      style={paletteVars}
    >
      {/* 工具条 */}
      <div className="flex items-center gap-2 border-b border-[color-mix(in_srgb,var(--code-fg)_9%,transparent)] bg-[color-mix(in_srgb,var(--code-fg)_4%,transparent)] px-2.5 py-1.5">
        {title ? (
          <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-[color-mix(in_srgb,var(--code-fg)_72%,transparent)]">
            {title}
          </span>
        ) : (
          <span className="min-w-0 flex-1 font-mono text-[10px] uppercase tracking-wide text-[color-mix(in_srgb,var(--code-fg)_45%,transparent)]">
            {LANG_LABEL[lang]}
          </span>
        )}
        {actions}
        <div className="flex shrink-0 items-center gap-0.5">
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => {
                  touched.current = true;
                  setFormatted((v) => !v);
                }}
                className={cn(
                  "flex h-6 w-6 items-center justify-center rounded-md transition-colors",
                  formatted
                    ? "bg-primary/15 text-primary"
                    : "text-[color-mix(in_srgb,var(--code-fg)_45%,transparent)] hover:bg-[color-mix(in_srgb,var(--code-fg)_8%,transparent)] hover:text-[color-mix(in_srgb,var(--code-fg)_72%,transparent)]"
                )}
              >
                <Sparkles className="h-3 w-3" />
              </button>
            </TooltipTrigger>
            <TooltipContent>{formatted ? t("code.unformat") : t("code.format")}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => {
                  touched.current = true;
                  setShowNum((v) => !v);
                }}
                className={cn(
                  "flex h-6 w-6 items-center justify-center rounded-md transition-colors",
                  showNum
                    ? "bg-primary/15 text-primary"
                    : "text-[color-mix(in_srgb,var(--code-fg)_45%,transparent)] hover:bg-[color-mix(in_srgb,var(--code-fg)_8%,transparent)] hover:text-[color-mix(in_srgb,var(--code-fg)_72%,transparent)]"
                )}
              >
                <Hash className="h-3 w-3" />
              </button>
            </TooltipTrigger>
            <TooltipContent>{t("code.lineNumbers")}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => {
                  touched.current = true;
                  setDoWrap((v) => !v);
                }}
                className={cn(
                  "flex h-6 w-6 items-center justify-center rounded-md transition-colors",
                  doWrap
                    ? "bg-primary/15 text-primary"
                    : "text-[color-mix(in_srgb,var(--code-fg)_45%,transparent)] hover:bg-[color-mix(in_srgb,var(--code-fg)_8%,transparent)] hover:text-[color-mix(in_srgb,var(--code-fg)_72%,transparent)]"
                )}
              >
                {doWrap ? <WrapText className="h-3 w-3" /> : <AlignLeft className="h-3 w-3" />}
              </button>
            </TooltipTrigger>
            <TooltipContent>{t("log.wrap")}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={copy}
                className="flex h-6 w-6 items-center justify-center rounded-md text-[color-mix(in_srgb,var(--code-fg)_45%,transparent)] transition-colors hover:bg-[color-mix(in_srgb,var(--code-fg)_8%,transparent)] hover:text-[color-mix(in_srgb,var(--code-fg)_72%,transparent)]"
              >
                {copied ? <Check className="h-3 w-3 text-running" /> : <Copy className="h-3 w-3" />}
              </button>
            </TooltipTrigger>
            <TooltipContent>{copied ? t("common.copied") : t("common.copy")}</TooltipContent>
          </Tooltip>
        </div>
      </div>

      {/* 代码区 */}
      <div
        className={cn("overflow-auto", compact ? "p-2" : "p-3")}
        style={{ maxHeight: maxHeight ?? undefined }}
      >
        <div className={cn(doWrap ? "whitespace-pre-wrap break-all" : "whitespace-pre")}>
          {tokenized.map((tokens, i) => (
            <div key={i} className="flex gap-3 leading-relaxed">
              {showNum && (
                <span className="w-7 shrink-0 select-none text-right font-mono text-[0.85em] tabular text-[color-mix(in_srgb,var(--code-fg)_30%,transparent)]">
                  {i + 1}
                </span>
              )}
              <code className="min-w-0 flex-1 font-mono text-[var(--code-fg)]">
                {tokens.map((tk, j) => (
                  <span key={j} className={tk.tk ? `ck-${tk.tk}` : undefined}>
                    {tk.text}
                  </span>
                ))}
              </code>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

/** 行内代码（路径 / 命令片段），统一走代码字体 */
export function InlineCode({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <code className={cn("rounded bg-card-2/70 px-1.5 py-0.5 font-mono text-[11.5px] text-secondary", className)}>
      {children}
    </code>
  );
}
