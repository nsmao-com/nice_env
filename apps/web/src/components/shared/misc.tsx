"use client";

import * as React from "react";
import { motion } from "motion/react";
import { Check, Copy } from "lucide-react";
import { useUI, useT } from "@/lib/store";
import { cn } from "@/lib/utils";
import { copyText } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/* ============ 复制按钮 ============ */
export function CopyButton({
  text,
  className,
  size = "icon-sm",
}: {
  text: string;
  className?: string;
  size?: "icon-sm" | "sm" | "default";
}) {
  const t = useT();
  const [copied, setCopied] = React.useState(false);
  return (
    <Button
      variant="ghost"
      size={size}
      className={cn("text-faint hover:text-foreground", className)}
      onClick={async (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (await copyText(text)) {
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        }
      }}
      title={t("common.copyTitle")}
      aria-label={t("common.copyTitle")}
    >
      {copied ? <Check className="h-3.5 w-3.5 text-running" /> : <Copy className="h-3.5 w-3.5" />}
    </Button>
  );
}

/* ============ 空状态：大图标 + 一个主 CTA ============ */
export function EmptyState({
  icon: Icon,
  title,
  hint,
  action,
  className,
}: {
  icon: React.ComponentType<{ className?: string; strokeWidth?: number }>;
  title: string;
  hint?: string;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <motion.div
      initial={{ opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3, ease: "easeOut" }}
      className={cn(
        "flex flex-col items-center justify-center gap-4 rounded-xl bg-fill/40 py-16",
        className
      )}
    >
      <div className="relative">
        <div className="absolute inset-0 rounded-full bg-primary/15 blur-2xl" />
        <div className="relative flex h-14 w-14 items-center justify-center rounded-2xl bg-card shadow-[var(--shadow-card)]">
          <Icon className="h-6 w-6 text-faint" strokeWidth={1.5} />
        </div>
      </div>
      <div className="flex flex-col items-center gap-1">
        <p className="text-sm font-medium text-secondary">{title}</p>
        {hint && <p className="max-w-sm text-center text-xs text-faint">{hint}</p>}
      </div>
      {action}
    </motion.div>
  );
}

/* ============ 危险操作二次确认 ============ */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmText,
  danger,
  loading,
  confirmDisabled,
  onCloseAutoFocus,
  onConfirm,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  confirmText?: string;
  danger?: boolean;
  loading?: boolean;
  confirmDisabled?: boolean;
  onCloseAutoFocus?: React.ComponentProps<typeof DialogContent>["onCloseAutoFocus"];
  onConfirm: () => void;
  children?: React.ReactNode;
}) {
  const t = useT();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent hideClose={loading} onCloseAutoFocus={onCloseAutoFocus} className="flex max-w-md max-h-[85dvh] flex-col overflow-hidden">
        <div className="min-h-0 min-w-0 space-y-4 overflow-y-auto">
          <DialogHeader>
            <DialogTitle className="pr-6 leading-snug [overflow-wrap:anywhere]">{title}</DialogTitle>
            {description && <DialogDescription className="[overflow-wrap:anywhere]">{description}</DialogDescription>}
          </DialogHeader>
          {children}
        </div>
        <DialogFooter className="shrink-0 flex-wrap">
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={loading}>
            {t("common.cancel")}
          </Button>
          <Button variant={danger ? "destructive" : "default"} onClick={onConfirm} disabled={loading || confirmDisabled}>
            {loading ? t("confirm.busy") : confirmText}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/* ============ 区块标题 ============ */
export function SectionHeader({
  title,
  hint,
  actions,
  className,
}: {
  title: string;
  hint?: string;
  actions?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex items-center justify-between gap-4", className)}>
      <div className="flex flex-col gap-0.5">
        <h2 className="text-[14.5px] font-semibold tracking-tight">{title}</h2>
        {hint && <p className="text-xs tabular text-faint">{hint}</p>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </div>
  );
}

/* ============ 迷你折线图（资源占用） ============ */
export function Sparkline({
  data,
  width = 220,
  height = 44,
  stroke = "var(--primary)",
  fill = true,
  max,
  className,
}: {
  data: number[];
  width?: number;
  height?: number;
  stroke?: string;
  fill?: boolean;
  max?: number;
  className?: string;
}) {
  if (data.length < 2) data = [0, 0];
  const hi = max ?? Math.max(...data, 1) * 1.15;
  const n = data.length;
  const step = width / (n - 1);
  const pts = data.map((v, i) => [i * step, height - (Math.min(v, hi) / hi) * height] as const);
  const path = pts.map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const area = `${path} L${width},${height} L0,${height} Z`;
  const gid = React.useId();
  return (
    <svg width={width} height={height} className={className} preserveAspectRatio="none">
      <defs>
        <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={stroke} stopOpacity="0.28" />
          <stop offset="100%" stopColor={stroke} stopOpacity="0" />
        </linearGradient>
      </defs>
      {fill && <path d={area} fill={`url(#${gid})`} />}
      <path d={path} fill="none" stroke={stroke} strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

/* ============ 自定义悬浮提示（替代原生 title） ============ */
export function Hint({
  content,
  side = "top",
  children,
}: {
  content: React.ReactNode;
  side?: "top" | "bottom" | "left" | "right";
  children: React.ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side={side}>{content}</TooltipContent>
    </Tooltip>
  );
}
