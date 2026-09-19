"use client";

import * as React from "react";
import { motion } from "motion/react";
import { cn } from "@/lib/utils";

/** 环形进度（下载/安装） */
export function RingProgress({
  value,
  size = 64,
  strokeWidth = 5,
  className,
  children,
  indeterminate,
}: {
  value: number;
  size?: number;
  strokeWidth?: number;
  className?: string;
  children?: React.ReactNode;
  indeterminate?: boolean;
}) {
  const r = (size - strokeWidth) / 2;
  const c = 2 * Math.PI * r;
  const pct = Math.min(100, Math.max(0, value));
  return (
    <div className={cn("relative inline-flex items-center justify-center", className)} style={{ width: size, height: size }}>
      <svg width={size} height={size} className="-rotate-90">
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          strokeWidth={strokeWidth}
          className="stroke-border"
        />
        <motion.circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          className="stroke-primary"
          initial={false}
          animate={{
            strokeDasharray: `${indeterminate ? c * 0.3 : (c * pct) / 100} ${c}`,
            rotate: indeterminate ? 360 : 0,
          }}
          transition={
            indeterminate
              ? { duration: 1.1, repeat: Infinity, ease: "linear" }
              : { duration: 0.3 }
          }
        />
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center">{children}</div>
    </div>
  );
}
