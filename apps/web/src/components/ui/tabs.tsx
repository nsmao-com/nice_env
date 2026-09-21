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
 * 分段控件（Segmented Control）。
 *
 * 刻意不做成 macOS 系统偏好设置那种「灰轨道 + 白滑块」——那是最容易被一眼
 * 认成系统原生控件的形态。这里改成：内凹轨道（inset 阴影而不是投影片）、
 * 悬浮滑块用卡片渐变 + 分层阴影 + 强调色描边，激活文字也走强调色，
 * 让「当前选中」在视觉上真正有重量。
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
          // p-[3px] + 内凹阴影：轨道看起来是「凹槽」，滑块浮在槽里
          "inline-flex min-h-[34px] flex-wrap items-center gap-y-[3px] rounded-xl border border-border/70 bg-card-2/60 p-[3px] text-muted",
          "shadow-[inset_0_1px_2px_rgba(28,25,23,0.045)]",
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
        "relative inline-flex h-7 cursor-pointer select-none items-center justify-center gap-1.5 whitespace-nowrap rounded-[9px] px-3 text-[13px] font-medium transition-colors duration-150",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--primary-ring)]",
        "disabled:pointer-events-none disabled:opacity-40",
        active ? "text-primary" : "text-muted hover:text-foreground",
        className
      )}
      {...props}
    >
      {/* 滑块：卡片渐变 + 分层阴影 + 强调色描边；spring 平滑滑到当前 trigger */}
      {active && (
        <motion.span
          aria-hidden
          layoutId={pill}
          className="card-fill absolute inset-0 rounded-[9px] shadow-[var(--thumb-shadow)] ring-1 ring-inset ring-[hsl(var(--accent-h)_62%_45%/0.3)]"
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
