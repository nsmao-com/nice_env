"use client";

import * as React from "react";
import { motion } from "motion/react";
import {
  AlertTriangle,
  ArrowDown,
  Copy,
  Hash,
  Pause,
  Play,
  ScrollText,
  Search,
  WrapText,
  X,
  Download,
  Loader2,
} from "lucide-react";
import { toast } from "sonner";
import { useT } from "@/lib/store";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import { toastError } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useLogTail } from "@/lib/hooks";
import {
  LEVEL_STYLE,
  parseLogLine,
  tokenClass,
  type LogLevel,
  type ParsedLogLine,
} from "@/lib/log-highlight";

type LevelFilter = "all" | "error" | "warn" | "info";

/**
 * 日志面板：级别高亮 + 关键字搜索 + 级别过滤 + 自动滚动 + 一键复制。
 * 长列表用 CSS contain + 限制渲染条数实现轻量虚拟化。
 */
export function LogPane({
  serviceId,
  className,
  emptyHint,
  height = 320,
  tailLines = 500,
  defaultAutoRefresh = true,
}: {
  serviceId: string | null;
  className?: string;
  emptyHint?: string;
  height?: number;
  tailLines?: number;
  defaultAutoRefresh?: boolean;
}) {
  const t = useT();
  const [exporting, setExporting] = React.useState(false);
  const [paused, setPaused] = React.useState(!defaultAutoRefresh);
  const [visibleLines, setVisibleLines] = React.useState(tailLines);
  const { lines, error } = useLogTail(serviceId, 1500, Math.max(visibleLines, tailLines), !paused);
  const [filter, setFilter] = React.useState<LevelFilter>("all");
  const [query, setQuery] = React.useState("");
  /** 命中关键字时高亮出来；不输入时不做二次渲染 */
  const [autoScroll, setAutoScroll] = React.useState(true);
  const [wrap, setWrap] = React.useState(true);
  const [showLineNumbers, setShowLineNumbers] = React.useState(false);
  const boxRef = React.useRef<HTMLDivElement>(null);

  /** 解析一次，过滤与渲染共用（避免每行解析两遍） */
  const parsed = React.useMemo<ParsedLogLine[]>(
    () => lines.map((l) => ({ ...parseLogLine(l), raw: l }) as ParsedLogLine & { raw: string }),
    [lines]
  );

  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    return parsed.filter((p) => {
      const raw = (p as ParsedLogLine & { raw: string }).raw.toLowerCase();
      if (filter === "error" && p.level !== "error") return false;
      if (filter === "warn" && p.level !== "warn" && p.level !== "error") return false;
      if (q && !raw.includes(q)) return false;
      return true;
    });
  }, [parsed, filter, query]);

  /** 级别计数（给过滤按钮显示徽标） */
  const counts = React.useMemo(() => {
    let err = 0;
    let warn = 0;
    for (const p of parsed) {
      if (p.level === "error") err += 1;
      else if (p.level === "warn") warn += 1;
    }
    return { err, warn,total: parsed.length };
  }, [parsed]);

  React.useEffect(() => {
    if (autoScroll && boxRef.current && !paused) {
      boxRef.current.scrollTop = boxRef.current.scrollHeight;
    }
  }, [filtered, autoScroll, paused]);

  const joined = React.useMemo(() => filtered.map((p) => (p as ParsedLogLine & { raw: string }).raw).join("\n"), [filtered]);

  /** 高亮查询命中：把 token 文本再切一层，命中的字串加背景 */
  const highlightQuery = (text: string): React.ReactNode => {
    const q = query.trim();
    if (!q) return text;
    const lower = text.toLowerCase();
    const needle = q.toLowerCase();
    const parts: React.ReactNode[] = [];
    let i = 0;
    let k = lower.indexOf(needle);
    if (k < 0) return text;
    while (k >= 0) {
      if (k > i) parts.push(text.slice(i, k));
      parts.push(
        <mark key={`${k}-${i}`} className="rounded-[3px] bg-primary/30 px-0.5 text-foreground">
          {text.slice(k, k + q.length)}
        </mark>
      );
      i = k + q.length;
      k = lower.indexOf(needle, i);
    }
    if (i < text.length) parts.push(text.slice(i));
    return parts;
  };

  /**
   * 导出当前视图。
   *
   * 用 `joined` 而不是重新拼 filtered —— joined 就是渲染用的那份原始文本
   * （无损，高亮只做着色不改内容），导出内容与屏幕上看到的逐字节一致。
   */
  const doExport = async () => {
    if (!serviceId || filtered.length === 0) return;
    setExporting(true);
    try {
      const text = joined.endsWith("\n") ? joined : joined + "\n";
      const path = await api.logExport(serviceId, text);
      toast.success(t("log.exported"), { description: path });
    } catch (e) {
      toastError(e);
    } finally {
      setExporting(false);
    }
  };

  return (
    <div className={cn("flex flex-col gap-2", className)}>
      {/* 工具条：级别过滤 + 搜索 + 操作 */}
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1">
          <FilterButton active={filter === "all"} onClick={() => setFilter("all")} label={t("log.allLevels")} count={counts.total} />
          <FilterButton
            active={filter === "error"}
            onClick={() => setFilter("error")}
            label={t("log.errors")}
            count={counts.err}
            tone="error"
            icon={<AlertTriangle className="h-3 w-3" />}
          />
          <FilterButton
            active={filter === "warn"}
            onClick={() => setFilter("warn")}
            label={t("log.warnings")}
            count={counts.warn}
            tone="warn"
          />
        </div>
        <div className="flex items-center gap-1">
          <div className="relative">
            <Search className="pointer-events-none absolute left-2 top-1/2 h-3 w-3 -translate-y-1/2 text-faint" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("log.searchPlaceholder")}
              className="h-7 w-44 pl-7 pr-6 text-[11.5px]"
            />
            {query && (
              <button
                type="button"
                onClick={() => setQuery("")}
                className="absolute right-1.5 top-1/2 -translate-y-1/2 text-faint hover:text-secondary"
              >
                <X className="h-3 w-3" />
              </button>
            )}
          </div>
          <Button
            size="icon-sm"
            variant="ghost"
            title={paused ? t("log.resume") : t("log.pause")}
            className={cn(paused && "text-warn")}
            onClick={() => setPaused((v) => !v)}
          >
            {paused ? <Play className="h-3.5 w-3.5" /> : <Pause className="h-3.5 w-3.5" />}
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            title={t("log.autoScroll")}
            className={cn(autoScroll && "text-primary")}
            onClick={() => setAutoScroll((v) => !v)}
          >
            <ArrowDown className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            title={t("log.wrap")}
            className={cn(wrap && "text-primary")}
            onClick={() => setWrap((v) => !v)}
          >
            <WrapText className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            title={t("code.lineNumbers")}
            className={cn(showLineNumbers && "text-primary")}
            onClick={() => setShowLineNumbers((v) => !v)}
          >
            <Hash className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            title={t("log.copyAll")}
            onClick={() => navigator.clipboard.writeText(joined)}
          >
            <Copy className="h-3.5 w-3.5" />
          </Button>
          {/* 导出：导出的正是当前过滤/搜索后的内容，而不是全量 —— 否则
              「我搜了 error 导出却是全部」会让人不信任这个功能 */}
          <Button
            size="icon-sm"
            variant="ghost"
            title={t("log.export")}
            disabled={filtered.length === 0}
            onClick={() => void doExport()}
          >
            {exporting ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Download className="h-3.5 w-3.5" />
            )}
          </Button>
        </div>
      </div>

      {/* 统计条：过滤结果数 / 暂停提示 */}
      <div className="flex items-center gap-2 text-[10.5px] text-faint">
        <ScrollText className="h-3 w-3" />
        <span>
          {filtered.length}
          {filtered.length !== parsed.length ? ` / ${parsed.length}` : ""} {t("log.lineCount")}
        </span>
        {paused && <span className="text-warn">· {t("log.pausedHint")}</span>}
        {query && <span className="text-primary/80">· “{query}”</span>}
      </div>

      <motion.div
        ref={boxRef}
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        onScroll={(e) => {
          // 用户手动往回滚 → 停掉自动滚动，避免「看不到自己在读什么」
          const el = e.currentTarget;
          const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
          if (!atBottom && autoScroll) setAutoScroll(false);
        }}
        className="overflow-y-auto rounded-xl border border-border bg-[#0A0C0F] p-3 font-mono leading-relaxed [contain:content]"
        style={{ height, fontSize: "var(--code-font-size)" }}
      >
        {error ? (
          <p className="text-error">
            {t("log.readFailed")}
            {error.message}
            {error.hint && <span className="block text-faint">{error.hint}</span>}
          </p>
        ) : filtered.length === 0 ? (
          <p className="text-faint">{lines.length > 0 ? t("log.noMatch") : emptyHint}</p>
        ) : (
          <>
            {filtered.length > 3000 && (
              <p className="mb-1 text-faint">{t("log.truncated")}</p>
            )}
            {filtered.slice(-3000).map((p, i) => {
              const style = LEVEL_STYLE[p.level];
              // 行号：过滤后重新编号（用户看到的就是当前视图的第几行）
              const lineNo = filtered.length - Math.min(3000, filtered.length) + i + 1;
              return (
                <div
                  key={i}
                  className={cn(
                    "group flex gap-2 py-[1px]",
                    wrap ? "whitespace-pre-wrap break-all" : "whitespace-pre"
                  )}
                >
                  {showLineNumbers && (
                    <span className="w-8 shrink-0 select-none text-right text-white/20 tabular">{lineNo}</span>
                  )}
                  {/* 级别色条：左侧 2px，扫一眼就能看出哪几行是错误 */}
                  <span
                    className={cn(
                      "mt-[3px] h-[13px] w-[2px] shrink-0 rounded-full",
                      p.level === "error" && "bg-error",
                      p.level === "warn" && "bg-warn",
                      p.level === "notice" && "bg-info",
                      (p.level === "info" || p.level === "none") && "bg-transparent"
                    )}
                  />
                  <span className={cn("min-w-0 flex-1", style.text)}>
                    {p.tokens.map((tk, j) => (
                      <span key={j} className={tokenClass(tk)}>
                        {highlightQuery(tk.text)}
                      </span>
                    ))}
                  </span>
                </div>
              );
            })}
          </>
        )}
      </motion.div>

      {/* 加载更多 */}
      <div className="flex items-center justify-between text-[10.5px] text-faint">
        <span>{t("log.tailHint")} {visibleLines}</span>
        <Button
          size="sm"
          variant="ghost"
          className="h-6 text-[10.5px]"
          onClick={() => setVisibleLines((v) => Math.min(v + 1000, 20000))}
        >
          {t("log.loadMore")}
        </Button>
      </div>
    </div>
  );
}

function FilterButton({
  active,
  onClick,
  label,
  count,
  tone,
  icon,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  count?: number;
  tone?: "error" | "warn";
  icon?: React.ReactNode;
}) {
  return (
    <Button
      size="sm"
      variant={active ? "secondary" : "ghost"}
      className={cn(
        "h-7 gap-1 text-xs",
        active && "text-foreground",
        tone === "error" && active && "text-error",
        tone === "warn" && active && "text-warn"
      )}
      onClick={onClick}
    >
      {icon}
      {label}
      {count != null && count > 0 && (
        <span
          className={cn(
            "rounded px-1 text-[10px] tabular",
            tone === "error" ? "bg-error/15 text-error" : tone === "warn" ? "bg-warn/15 text-warn" : "bg-card-2 text-faint"
          )}
        >
          {count}
        </span>
      )}
    </Button>
  );
}
