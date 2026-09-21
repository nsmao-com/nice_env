import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

/**
 * 徽标 —— Apple 风格：胶囊形、语义色淡底 + 语义色文字（tinted fill），
 * 不用 1px 灰框；颜色只表达状态，不做大面积高饱和。
 */
const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-full px-2 py-px text-[11px] font-medium leading-[18px] transition-colors whitespace-nowrap tabular",
  {
    variants: {
      variant: {
        default: "bg-primary-soft text-primary",
        running: "bg-running-soft text-running",
        error: "bg-error-soft text-error",
        warn: "bg-warn-soft text-warn",
        info: "bg-info-soft text-info",
        outline: "bg-fill text-muted",
        muted: "bg-fill text-secondary",
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
