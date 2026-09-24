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
 * 分段控件（Segmented Control）—— iOS 系统形态：
 * 轨道 = systemFill 灰胶囊；滑块 = 白色（深色下抬升一级）胶囊 +
 * 极淡投影，spring 滑到当前项；激活文字 = label 色。无边框、无强调色描边。
 *
 * 每个 TabsList 的滑块 layoutId 独立（useId），一个 Tabs 下可以放多个
 * TabsList：激活滑块只会出现在包含激活项的那条轨道里。
 * 页签过多时（如套件页分类）不要让单条轨道折行 —— 折行后首行第一个 /
 * 末行最后一个会顶着轨道圆角，显得突兀；把页签拆成几行、每行一个
 * TabsList，每条轨道都是完整的胶囊。
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
          // p-[2px]：胶囊轨道包胶囊滑块，滑块半径 = 轨道半径 − 2（同心）
          "inline-flex min-h-8 items-center rounded-full bg-fill p-[2px] text-muted",
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
        // h-7 而非 h-full：折行成多行后每一行都要保持固定高度；
        // 圆角 = 高度一半（胶囊），与轨道同心
        "relative inline-flex h-7 cursor-pointer select-none items-center justify-center gap-1.5 whitespace-nowrap rounded-full px-3 text-[13px] font-medium transition-colors duration-150 active:scale-[0.97]",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--primary-ring)]",
        "disabled:pointer-events-none disabled:opacity-40",
        active ? "text-foreground" : "text-muted hover:text-foreground",
        className
      )}
      {...props}
    >
      {/* 滑块：白胶囊（深色抬升）+ 极淡投影；spring 平滑滑到当前 trigger */}
      {active && (
        <motion.span
          aria-hidden
          layoutId={pill}
          className="absolute inset-0 rounded-full bg-card shadow-[var(--thumb-shadow)]"
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
