import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * 输入框 —— Apple：systemFill 填充 + 1px 发丝描边；聚焦只把描边转成
 * 主题色（--primary 跟随设置里的强调色），不画阴影/光环；radius 12（输入框档）。
 */
const Input = React.forwardRef<HTMLInputElement, React.ComponentProps<"input">>(
  ({ className, type, ...props }, ref) => (
    <input
      type={type}
      ref={ref}
      className={cn(
        "flex h-9 w-full rounded-md border border-border bg-fill px-3 py-1 text-sm text-foreground transition-[border-color,background-color] placeholder:text-faint focus-visible:border-primary focus-visible:bg-card focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
);
Input.displayName = "Input";

export { Input };
