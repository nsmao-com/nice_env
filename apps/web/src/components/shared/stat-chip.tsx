"use client";

import * as React from "react";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/** 信息小芯片：icon + label */
export function StatChip({
  icon: Icon,
  children,
  className,
  title,
}: {
  icon: React.ComponentType<{ className?: string }>;
  children: React.ReactNode;
  className?: string;
  title?: string;
}) {
  const body = (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md bg-card-2/70 px-2 py-1 text-[11px] text-muted tabular",
        className
      )}
    >
      <Icon className="h-3 w-3 shrink-0" />
      {children}
    </span>
  );
  if (!title) return body;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{body}</TooltipTrigger>
      <TooltipContent>{title}</TooltipContent>
    </Tooltip>
  );
}
