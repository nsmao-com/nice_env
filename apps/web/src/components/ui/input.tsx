import * as React from "react";
import { cn } from "@/lib/utils";

const Input = React.forwardRef<HTMLInputElement, React.ComponentProps<"input">>(
  ({ className, type, ...props }, ref) => (
    <input
      type={type}
      ref={ref}
      className={cn(
        "flex h-9 w-full rounded-lg border border-border bg-card-2/40 px-3 py-1 text-sm text-foreground shadow-[inset_0_1px_2px_rgba(28,25,23,0.03)] transition-[border-color,box-shadow,background-color] placeholder:text-faint focus-visible:border-[hsl(var(--accent-h)_62%_45%/0.5)] focus-visible:bg-card focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-[var(--primary-ring)] disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
);
Input.displayName = "Input";

export { Input };
