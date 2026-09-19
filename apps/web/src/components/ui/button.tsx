import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-lg text-sm font-medium transition-all outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0 active:scale-[0.98] cursor-pointer select-none",
  {
    variants: {
      variant: {
        default:
          "bg-primary text-primary-fg shadow-sm hover:bg-primary/90",
        secondary:
          "bg-card-2 text-foreground border border-border hover:bg-card-2/70 hover:border-border-strong",
        outline:
          "border border-border bg-transparent text-foreground hover:bg-card-2 hover:border-border-strong",
        ghost: "text-secondary hover:bg-card-2 hover:text-foreground",
        destructive:
          "bg-error text-white shadow-sm hover:bg-error/90",
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
