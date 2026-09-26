/* ============================================================
   日志高亮：把一行原始日志拆成带语义的片段，供 UI 上色。
   纯函数、无依赖，便于单测；所有服务（nginx/mysql/php/redis/…）
   的日志格式差异集中在 detect 规则里，组件层只负责渲染。
   ============================================================ */

export type LogLevel = "error" | "warn" | "info" | "debug" | "trace" | "notice" | "none";

export interface LogToken {
  kind:
    | "timestamp"
    | "level"
    | "pid"
    | "httpStatus"
    | "httpMethod"
    | "path"
    | "ip"
    | "number"
    | "duration"
    | "key"
    | "string"
    | "text";
  text: string;
  /** 仅 level 片段有值 */
  level?: LogLevel;
}

export interface ParsedLogLine {
  tokens: LogToken[];
  level: LogLevel;
  /** 行内出现的 HTTP 状态码（用于「只看 5xx」这类过滤） */
  httpStatus?: number;
  timestamp?: string;
}

/* ---------- 级别识别 ---------- */

const LEVEL_PATTERNS: { re: RegExp; level: LogLevel }[] = [
  { re: /\[\s*(fatal|critical|crit|panic)\s*\]|\[\s*FATAL\s*\]|\bfatal\b|\bpanic\b/i, level: "error" },
  { re: /\[\s*error\s*\]|\berror\b|\bERR\b|\[ERR\]|\bsevere\b/i, level: "error" },
  { re: /\[\s*warn(ing)?\s*\]|\bwarn(ing)?\b/i, level: "warn" },
  { re: /\[\s*notice\s*\]|\bnotice\b/i, level: "notice" },
  { re: /\[\s*debug\s*\]|\bdebug\b/i, level: "debug" },
  { re: /\[\s*trace\s*\]|\btrace\b/i, level: "trace" },
  { re: /\[\s*info(rmation)?\s*\]|\binfo\b/i, level: "info" },
];

// 服务管理器的 OUT/ERR 标记表示输出流，不代表服务自身的日志级别。
const STREAM_PREFIX = /^\[\d{4}-\d{2}-\d{2}[^\]]*\]\s+\[(OUT|ERR)\]\s*/;
const EXPLICIT_LEVEL = /\[\s*(fatal|critical|crit|panic|error|err|severe|warning|warn|notice|debug|trace|info|information|system)\s*\]|\blevel\s*[=:]\s*["']?(fatal|critical|crit|panic|error|err|severe|warning|warn|notice|debug|trace|info|information)\b/i;

/** 优先使用服务明确声明的级别，避免正文中的 error 或 stderr 前缀误报。 */
export function detectLevel(line: string): LogLevel {
  const stream = STREAM_PREFIX.exec(line);
  const payload = stream ? line.slice(stream[0].length) : line;
  const explicit = EXPLICIT_LEVEL.exec(payload);
  if (explicit) {
    const value = (explicit[1] ?? explicit[2]).toLowerCase();
    if (value === "system" || value === "information") return "info";
    for (const p of LEVEL_PATTERNS) {
      if (p.re.test(value)) return p.level;
    }
  }
  for (const p of LEVEL_PATTERNS) {
    if (p.re.test(payload)) return p.level;
  }
  if (stream?.[1] === "ERR") return "error";
  return "none";
}

/* ---------- 词法切分 ---------- */

/**
 * 一行日志 → token 列表。
 * 设计原则：**永不丢字符**——无法识别的部分一律原样落进 text token，
 * 保证高亮只是「着色」，不会改变日志内容（复制出来必须和原始一致）。
 *
 * 备选项顺序很关键：带单位的时长（12ms）必须先于裸数字匹配，
 * 否则会被切成 "12" + "ms" 两个 token，状态码判断也会串味。
 */
export function tokenizeLog(line: string): LogToken[] {
  const tokens: LogToken[] = [];
  const streamPrefixLength = STREAM_PREFIX.exec(line)?.[0].length ?? 0;
  const atom = new RegExp(
    [
      // 1 时间戳（方括号包住的 / 裸露的 / 只有时分秒）
      String.raw`(\[\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\]|\d{4}[-/]\d{2}[-/]\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?|\d{2}:\d{2}:\d{2}(?:[.,]\d+)?)`,
      // 2 其它方括号段（级别 / pid / [System] 等标记）
      String.raw`(\[[^\]]{0,64}\])`,
      // 3 引号串
      String.raw`("(?:[^"\\]|\\.)*")`,
      // 4 时长（必须在裸数字前）
      String.raw`(\b\d+(?:\.\d+)?(?:ms|us|µs|ns|MB|KB|GB|s)\b)`,
      // 5 IPv4
      String.raw`(\b\d{1,3}(?:\.\d{1,3}){3}\b)`,
      // 6 HTTP 方法 / 大写级别词
      String.raw`(\b(?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|CONNECT|TRACE|INFO|WARN|WARNING|ERROR|ERR|FATAL|DEBUG|NOTICE)\b)`,
      // 7 :端口
      String.raw`(:\d{2,5}\b)`,
      // 8 路径
      String.raw`(\/(?:[\w.\-~%]+(?:\/[\w.\-~%]*)*)?)`,
      // 9 key=value
      String.raw`(\b[A-Za-z_][\w.\-]*=[^\s,;]+)`,
      // 10 裸数字（状态码 / pid / 计数）
      String.raw`(\b\d+(?:\.\d+)?\b)`,
    ].join("|"),
    "g"
  );

  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = atom.exec(line)) !== null) {
    if (m.index > last) {
      tokens.push({ kind: "text", text: line.slice(last, m.index) });
    }
    const raw = m[0];
    const kind: LogToken["kind"] = m[1]
      ? "timestamp"
      : m[2]
        ? bracketKind(m[2])
        : m[3]
          ? "string"
          : m[4]
            ? "duration"
            : m[5]
              ? "ip"
              : m[6]
                ? "httpMethod"
                : m[7]
                  ? "pid"
                  : m[8]
                    ? "path"
                    : m[9]
                      ? "key"
                      : "pid";

    const token: LogToken = { kind, text: raw };
    if (kind === "level") {
      if (m.index < streamPrefixLength) {
        token.kind = "key";
      } else {
        token.level = detectLevel(raw);
      }
    }
    if (kind === "httpMethod") {
      const lv = detectLevel(raw);
      if (lv !== "none") {
        token.kind = "level";
        token.level = lv;
      }
    }
    if (kind === "pid" && !raw.startsWith(":")) {
      // 裸数字：三位且在 HTTP 状态区间内 → 当作状态码
      const n = Number(raw);
      if (Number.isInteger(n) && n >= 100 && n <= 599 && raw.length === 3) {
        token.kind = "httpStatus";
      }
    }
    tokens.push(token);
    last = m.index + raw.length;
  }
  if (last < line.length) {
    tokens.push({ kind: "text", text: line.slice(last) });
  }
  if (tokens.length === 0) {
    tokens.push({ kind: "text", text: line });
  }
  return tokens;
}

/** 方括号段：可能是级别（[error]）、pid（[43#0]）或时间戳（[2026-09-19 10:02:11]） */
function bracketKind(raw: string): LogToken["kind"] {
  const inner = raw.slice(1, -1).trim();
  if (/^\d{4}-\d{2}-\d{2}/.test(inner)) return "timestamp";
  const lv = detectLevel(inner);
  if (lv !== "none") return "level";
  if (/^\d+(#\d+)?$/.test(inner)) return "pid";
  // mysql 的 [System] / [Server] / [MY-010931] 等标记
  if (/^[A-Z][\w-]*$/.test(inner)) return "level";
  return "text";
}

/** 完整解析一行：token 化 + 级别 + 状态码 + 时间戳抽取 */
export function parseLogLine(line: string): ParsedLogLine {
  const tokens = tokenizeLog(line);
  const level = detectLevel(line);
  const ts = tokens.find((t) => t.kind === "timestamp")?.text.replace(/^\[|\]$/g, "");

  let httpStatus: number | undefined;
  for (const t of tokens) {
    if (t.kind === "httpStatus") {
      const n = Number(t.text);
      if (n >= 100 && n <= 599) httpStatus = n;
    }
  }
  return { tokens, level, httpStatus, timestamp: ts };
}

/* ---------- 级别配色（与代码背景使用同一套变量） ---------- */

export const LEVEL_STYLE: Record<LogLevel, { text: string; badge: string; label: string }> = {
  error: { text: "text-[color:var(--code-error)]", badge: "bg-error/15 text-error border-error/30", label: "ERROR" },
  warn: { text: "text-[color:var(--code-warn)]", badge: "bg-warn/15 text-warn border-warn/30", label: "WARN" },
  notice: { text: "text-[color:var(--code-key)]", badge: "bg-info/15 text-info border-info/30", label: "NOTICE" },
  info: { text: "text-[color:var(--code-fg)]", badge: "bg-card-2 text-secondary border-border", label: "INFO" },
  debug: { text: "text-[color:var(--code-muted)]", badge: "bg-card-2 text-faint border-border", label: "DEBUG" },
  trace: { text: "text-[color:var(--code-muted)]", badge: "bg-card-2 text-faint/70 border-border", label: "TRACE" },
  none: { text: "text-[color:var(--code-fg)]", badge: "bg-card-2 text-faint border-border", label: "" },
};

/** token → class（时间戳弱化、级别加重、状态码按 2xx/3xx/4xx/5xx 分色） */
export function tokenClass(t: LogToken): string {
  switch (t.kind) {
    case "timestamp":
      return "text-[color:var(--code-muted)]";
    case "level":
      return LEVEL_STYLE[t.level ?? "none"].text + " font-medium";
    case "httpStatus": {
      const n = Number(t.text);
      if (n >= 500) return "text-[color:var(--code-error)] font-semibold";
      if (n >= 400) return "text-[color:var(--code-warn)] font-medium";
      if (n >= 300) return "text-[color:var(--code-key)]";
      return "text-[color:var(--code-string)]";
    }
    case "httpMethod":
      return "text-[color:var(--code-keyword)] font-medium";
    case "path":
      return "text-[color:var(--code-key)]";
    case "ip":
      return "text-[color:var(--code-variable)]";
    case "duration":
      return "text-[color:var(--code-number)]";
    case "string":
      return "text-[color:var(--code-string)]";
    case "key":
      return "text-[color:var(--code-muted)]";
    case "pid":
      return "text-[color:var(--code-muted)]";
    default:
      return "text-[color:var(--code-fg)]";
  }
}
