import * as React from "react";
import { cn } from "@/lib/utils";

function Separator({
  className,
  orientation = "horizontal",
  ...props
}: React.HTMLAttributes<HTMLDivElement> & { orientation?: "horizontal" | "vertical" }) {
  return (
    <div
      role="separator"
      className={cn(
        "shrink-0 bg-border",
        orientation === "horizontal" ? "h-px w-full" : "h-full w-px",
        className
      )}
      {...props}
    />
  );
}

function Skeleton({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("animate-pulse rounded-lg bg-card-2", className)} {...props} />;
}

function Progress({
  value,
  className,
  indeterminate,
}: {
  value?: number;
  className?: string;
  indeterminate?: boolean;
}) {
  return (
    <div className={cn("h-1.5 w-full overflow-hidden rounded-full bg-card-2", className)}>
      {indeterminate ? (
        <div className="h-full w-1/3 animate-progress-slide rounded-full bg-primary" />
      ) : (
        <div
          className="h-full rounded-full bg-primary transition-all duration-300"
          style={{ width: `${Math.min(100, Math.max(0, value ?? 0))}%` }}
        />
      )}
    </div>
  );
}

function Kbd({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        "pointer-events-none inline-flex h-5 select-none items-center gap-1 rounded border border-border bg-card-2 px-1.5 font-mono text-[10px] font-medium text-muted",
        className
      )}
    >
      {children}
    </kbd>
  );
}

export { Separator, Skeleton, Progress, Kbd };
