"use client";

import { motion } from "motion/react";
import { cn } from "@/lib/utils";
import type { ServiceState } from "@nsb/schema";

/** 状态灯：running=翠绿微呼吸，error=脉冲红，其余中性 */
export function StatusLight({
  state,
  className,
  size = 8,
}: {
  state: ServiceState | SiteStateLite;
  className?: string;
  size?: number;
}) {
  const running = state === "running";
  const error = state === "error";
  return (
    <span className={cn("relative inline-flex items-center justify-center", className)} style={{ width: size, height: size }}>
      {running && (
        <motion.span
          className="absolute inset-0 rounded-full bg-running"
          animate={{ scale: [1, 1.9], opacity: [0.5, 0] }}
          transition={{ duration: 1.8, repeat: Infinity, ease: "easeOut" }}
        />
      )}
      <span
        className={cn(
          "relative rounded-full",
          running && "bg-running shadow-[0_0_8px_0_var(--running)]",
          error && "bg-error",
          (state === "stopped" || state === "unknown") && "bg-faint/60",
          state === "starting" && "bg-warn animate-pulse",
          state === "stopping" && "bg-warn/60 animate-pulse"
        )}
        style={{ width: size, height: size }}
      />
    </span>
  );
}

type SiteStateLite = "running" | "stopped" | "error" | "unconfigured";
