import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

/**
 * 按钮 —— Apple HIG：
 * 全部胶囊（Capsule，半径 = 高度一半）；主按钮 = 主题色实底 + 白字 +
 * 一层极淡内高光；次按钮 = systemFill 灰底；tinted = 淡主题色底 + 主题色字；
 * 破坏性 = 红胶囊，视觉重量不与主按钮并驾。按压缩放 0.97（Apple 触感），
 * 不做 Material 式悬停上浮与厚投影。
 */
const buttonVariants = cva(
  "relative inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-full text-sm font-medium outline-none transition-[background-color,box-shadow,transform,color] duration-150 active:scale-[0.97] disabled:pointer-events-none disabled:opacity-45 disabled:shadow-none disabled:active:scale-100 [&_svg]:pointer-events-none [&_svg]:shrink-0 cursor-pointer select-none",
  {
    variants: {
      variant: {
        default: "btn-solid bg-primary text-primary-fg hover:bg-[var(--accent-solid-hover)]",
        secondary: "bg-fill text-foreground hover:bg-card-2",
        tinted: "bg-primary-soft text-primary hover:brightness-[0.97]",
        outline: "bg-transparent text-secondary hover:bg-fill hover:text-foreground",
        ghost: "text-secondary hover:bg-fill hover:text-foreground",
        destructive:
          "bg-error text-white shadow-[inset_0_0.5px_0_rgba(255,255,255,0.22)] hover:brightness-105",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4",
        sm: "h-8 px-3 text-xs",
        lg: "h-10 px-5",
        icon: "h-8 w-8",
        "icon-sm": "h-7 w-7",
      },
    },
    defaultVariants: { variant: "default", size: "default" },
  }
);

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean;
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, title, ...props }, ref) => {
    const Comp = asChild ? Slot : "button";
    const btn = (
      <Comp ref={ref} className={cn(buttonVariants({ variant, size, className }))} {...props} />
    );
    /* title 不落到原生 DOM，改为应用统一的 Tooltip */
    if (!title) return btn;
    return (
      <Tooltip>
        <TooltipTrigger asChild>{btn}</TooltipTrigger>
        <TooltipContent>{title}</TooltipContent>
      </Tooltip>
    );
  }
);
Button.displayName = "Button";

export { Button, buttonVariants };
