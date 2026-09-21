import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-md border px-1.5 py-px text-[11px] font-medium leading-[18px] transition-colors whitespace-nowrap tabular",
  {
    variants: {
      variant: {
        default: "border-[hsl(var(--accent-h)_62%_45%/0.18)] bg-primary-soft text-primary",
        running: "border-running/20 bg-running-soft text-running",
        error: "border-error/20 bg-error-soft text-error",
        warn: "border-warn/20 bg-warn-soft text-warn",
        info: "border-info/20 bg-info-soft text-info",
        outline: "border-border text-muted",
        muted: "border-transparent bg-card-2 text-muted",
      },
    },
    defaultVariants: { variant: "default" },
  }
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {}

function Badge({ className, variant, ...props }: BadgeProps) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />;
}

export { Badge, badgeVariants };
