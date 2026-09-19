"use client";

import * as React from "react";
import { motion, AnimatePresence } from "motion/react";
import { Check } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * 服务开关：thumb 与光晕同步；成功后有一次极短的 check 动画。
 * 不用原生 Switch —— 需要自定义动画。
 */
export function ServiceSwitch({
  checked,
  onCheckedChange,
  disabled,
  busy,
  className,
}: {
  checked: boolean;
  onCheckedChange: (next: boolean) => void;
  disabled?: boolean;
  busy?: boolean;
  className?: string;
}) {
  const [showCheck, setShowCheck] = React.useState(false);

  return (
    <button
      role="switch"
      aria-checked={checked}
      disabled={disabled || busy}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onCheckedChange(!checked);
      }}
      className={cn(
        "relative inline-flex h-[22px] w-[40px] shrink-0 cursor-pointer items-center rounded-full transition-all duration-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-not-allowed disabled:opacity-60",
        checked
          ? "bg-running/90 shadow-[0_0_12px_0_hsl(160_84%_65%/0.35)]"
          : "bg-border-strong",
        className
      )}
    >
      <motion.span
        layout
        transition={{ type: "spring", stiffness: 700, damping: 34 }}
        className={cn(
          "absolute flex h-[18px] w-[18px] items-center justify-center rounded-full bg-white shadow",
          checked ? "right-[2px]" : "left-[2px]"
        )}
      >
        <AnimatePresence>
          {showCheck && (
            <motion.span
              initial={{ scale: 0, opacity: 0 }}
              animate={{ scale: 1, opacity: 1 }}
              exit={{ scale: 0, opacity: 0 }}
              transition={{ duration: 0.18 }}
            >
              <Check className="h-3 w-3 text-running" strokeWidth={3} />
            </motion.span>
          )}
        </AnimatePresence>
      </motion.span>
      {busy && (
        <span className="absolute inset-0 animate-pulse rounded-full ring-2 ring-warn/40" />
      )}
    </button>
  );
}

/** 由外部在操作成功后触发 check 动画 */
export function useSwitchCheck() {
  const [key, setKey] = React.useState(0);
  return {
    fire: () => setKey((k) => k + 1),
    key,
  };
}
