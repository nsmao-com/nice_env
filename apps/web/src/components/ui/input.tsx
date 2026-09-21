import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * 输入框 —— Apple：只用 systemFill 填充（#E5E5EA 档 / 深色 #2C2C2E 档），
 * 不画粗边框；聚焦 = 主题色柔环；radius 12（输入框档）。
 */
const Input = React.forwardRef<HTMLInputElement, React.ComponentProps<"input">>(
  ({ className, type, ...props }, ref) => (
    <input
      type={type}
      ref={ref}
      className={cn(
        "flex h-9 w-full rounded-md bg-fill px-3 py-1 text-sm text-foreground transition-[box-shadow,background-color] placeholder:text-faint focus-visible:bg-card focus-visible:shadow-[0_0_0_2.5px_var(--primary-ring)] focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
);
Input.displayName = "Input";

export { Input };
