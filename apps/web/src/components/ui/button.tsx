import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

/**
 * 按钮：主色走强调色（--primary 由外观层写入），实心表面带一层内高光，
 * 悬停时轻微上浮。克制但有“实体按键”的手感，避免默认 shadcn 的塑料感。
 */
const buttonVariants = cva(
  "relative inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-lg text-sm font-medium outline-none transition-[background-color,box-shadow,transform,border-color,color] duration-150 disabled:pointer-events-none disabled:opacity-45 disabled:shadow-none [&_svg]:pointer-events-none [&_svg]:shrink-0 cursor-pointer select-none",
  {
    variants: {
      variant: {
        default:
          "btn-solid bg-primary text-primary-fg hover:-translate-y-px hover:brightness-110 active:translate-y-0 active:brightness-95",
        secondary:
          "card-fill border border-border text-foreground shadow-[var(--shadow-card)] hover:border-border-strong hover:-translate-y-px active:translate-y-0",
        outline:
          "border border-border bg-transparent text-secondary hover:border-border-strong hover:bg-card-2/70 hover:text-foreground",
        ghost: "text-secondary hover:bg-card-2/80 hover:text-foreground",
        destructive:
          "bg-error text-white shadow-[inset_0_1px_0_rgba(255,255,255,0.16),0_1px_2px_rgba(28,25,23,0.16)] hover:-translate-y-px hover:brightness-110 active:translate-y-0",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4",
        sm: "h-8 px-3 text-xs rounded-md",
        lg: "h-10 px-5",
        icon: "h-8 w-8 rounded-md",
        "icon-sm": "h-7 w-7 rounded-md",
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
