"use client";

import * as React from "react";
import * as TabsPrimitive from "@radix-ui/react-tabs";
import { motion } from "motion/react";
import { cn } from "@/lib/utils";

/* 当前激活值（驱动滑块）；滑块 layoutId 用 useId 命名空间隔离多组 Tabs */
const ValueCtx = React.createContext<string>("");
const PillCtx = React.createContext<string>("tab-pill");
PillCtx.displayName = "TabPillCtx";

const Tabs = ({
  value: controlled,
  defaultValue = "",
  onValueChange,
  ...props
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.Root>) => {
  const [inner, setInner] = React.useState(controlled ?? defaultValue);
  React.useEffect(() => {
    if (controlled !== undefined) setInner(controlled);
  }, [controlled]);
  const handle = (v: string) => {
    setInner(v);
    onValueChange?.(v);
  };
  return (
    <ValueCtx.Provider value={inner}>
      <TabsPrimitive.Root value={inner} onValueChange={handle} {...props} />
    </ValueCtx.Provider>
  );
};

/**
 * Apple 分段控件（Segmented Control）。
 * 视觉按 macOS 系统偏好设置实现：低饱和圆角轨道 + 白色浮动滑块
 * （1px 极淡描边 + 轻微投影），滑块用 spring 动画平滑移动。
 *
 * 溢出处理：项数多时（如套件页 13+ 个分类）轨道自动折行成多行。
 * 不采用横向滚动 —— 隐藏滚动条后普通鼠标滚轮并不会横滚，等于把
 * 尾部几个分类藏了起来；折行能让所有页签始终可见，一次点击即达。
 * 折行不影响滑块：layoutId 动画会跨行平滑移动。
 */
const TabsList = React.forwardRef<
  React.ComponentRef<typeof TabsPrimitive.List>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>
>(({ className, children, ...props }, ref) => {
  const pill = React.useId();
  return (
    <PillCtx.Provider value={pill}>
      <TabsPrimitive.List
        ref={ref}
        className={cn(
          // p-[2px] / rounded-[9px] 贴近系统控件的紧凑比例；
          // 单行时高度 2+28+2=32px，与固定 h-8 观感一致
          "inline-flex min-h-8 flex-wrap items-center gap-y-[3px] rounded-[9px] bg-seg-track p-[2px] text-muted",
          className
        )}
        {...props}
      >
        {children}
      </TabsPrimitive.List>
    </PillCtx.Provider>
  );
});
TabsList.displayName = "TabsList";

const TabsTrigger = React.forwardRef<
  React.ComponentRef<typeof TabsPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>
>(({ className, children, value, ...props }, ref) => {
  const pill = React.useContext(PillCtx);
  const active = React.useContext(ValueCtx) === value;
  return (
    <TabsPrimitive.Trigger
      ref={ref}
      value={value}
      className={cn(
        // h-7 而非 h-full：折行成多行后每一行都要保持固定高度
        "relative inline-flex h-7 cursor-pointer select-none items-center justify-center gap-1.5 whitespace-nowrap rounded-[7px] px-3 text-[13px] font-medium transition-colors duration-150",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-apple-blue/40",
        "disabled:pointer-events-none disabled:opacity-40",
        active ? "text-seg-active" : "text-muted hover:text-foreground",
        className
      )}
      {...props}
    >
      {/* 滑块：白卡片 + 极淡描边 + 柔和投影，spring 平滑滑到当前 trigger */}
      {active && (
        <motion.span
          aria-hidden
          layoutId={pill}
          className="absolute inset-0 rounded-[7px] bg-seg-thumb ring-1 ring-inset ring-[var(--seg-thumb-ring)]"
          style={{ boxShadow: "var(--seg-thumb-shadow)" }}
          transition={{ type: "spring", stiffness: 520, damping: 40, mass: 0.7 }}
        />
      )}
      <span className="relative z-10 inline-flex items-center gap-1.5">{children}</span>
    </TabsPrimitive.Trigger>
  );
});
TabsTrigger.displayName = "TabsTrigger";

const TabsContent = React.forwardRef<
  React.ComponentRef<typeof TabsPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Content
    ref={ref}
    className={cn("mt-2 focus-visible:outline-none", className)}
    {...props}
  />
));
TabsContent.displayName = "TabsContent";

export { Tabs, TabsList, TabsTrigger, TabsContent };
