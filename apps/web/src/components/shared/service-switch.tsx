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
        "relative inline-flex h-[24px] w-[40px] shrink-0 cursor-pointer items-center rounded-full transition-colors duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--primary-ring)] disabled:cursor-not-allowed disabled:opacity-60",
        // 开启态走强调色（systemBlue 档）而非固定绿，与主按钮同一套色；
        // Apple 开关不做凹槽与光晕，层次全交给白色 thumb 的极淡投影
        checked ? "bg-primary" : "bg-fill",
        className
      )}
    >
      <motion.span
        layout
        transition={{ type: "spring", stiffness: 700, damping: 34 }}
        className={cn(
          "absolute flex h-[20px] w-[20px] items-center justify-center rounded-full bg-white shadow-[0_1px_2px_rgba(0,0,0,0.18),0_0_0_0.5px_rgba(0,0,0,0.04)]",
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
              <Check className="h-3 w-3 text-primary" strokeWidth={3} />
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
