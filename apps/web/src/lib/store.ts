"use client";

import * as React from "react";

import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { Lang, TKey } from "./i18n";
import { translate } from "./i18n";

interface UIState {
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
  commandOpen: boolean;
  setCommandOpen: (open: boolean) => void;
  wizardOpen: boolean;
  setWizardOpen: (open: boolean) => void;
  onboardingOpen: boolean;
  setOnboardingOpen: (open: boolean) => void;
  /** 服务列表展示形态：卡片（信息全）/ 列表（一屏看更多） */
  serviceView: ServiceView;
  setServiceView: (v: ServiceView) => void;
  lang: Lang;
  setLang: (l: Lang) => void;
  t: (key: TKey) => string;
  /** 代码块默认形态（由设置页同步；避免每个 CodeBlock 都去查一次设置） */
  codeDefaults: { lineNumbers: boolean; wrap: boolean };
  setCodeDefaults: (v: { lineNumbers: boolean; wrap: boolean }) => void;

  /* ---- 跨页意图：命令面板/托盘等入口让目标页自动打开某个面板 ----
     用 store 而不是 URL query，因为桌面端是静态导出，
     query 参数在客户端路由下不一定触发页面的 effect。
     消费方读到后应立即清除，避免以后每次进这个页面都弹一次。 */
  /** 站点页：打开「扫描项目」对话框 */
  pendingScan: boolean;
  requestScan: () => void;
  consumeScan: () => void;
  /** 工具箱页：定位到诊断报告 / 配置编辑器 */
  pendingTool: "diagnostics" | "config" | null;
  requestTool: (which: "diagnostics" | "config") => void;
  consumeTool: () => void;
}

export type ServiceView = "card" | "list";

export const useUI = create<UIState>()(
  persist(
    (set, get) => ({
      sidebarCollapsed: false,
      toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
      commandOpen: false,
      setCommandOpen: (open) => set({ commandOpen: open }),
      wizardOpen: false,
      setWizardOpen: (open) => set({ wizardOpen: open }),
      onboardingOpen: false,
      setOnboardingOpen: (open) => set({ onboardingOpen: open }),
      serviceView: "card",
      setServiceView: (v) => set({ serviceView: v }),
      lang: "zh",
      setLang: (l) => set({ lang: l }),
      t: (key) => translate(get().lang, key),
      codeDefaults: { lineNumbers: true, wrap: false },
      setCodeDefaults: (v) => set({ codeDefaults: v }),
      pendingScan: false,
      requestScan: () => set({ pendingScan: true }),
      consumeScan: () => set({ pendingScan: false }),
      pendingTool: null,
      requestTool: (which) => set({ pendingTool: which }),
      consumeTool: () => set({ pendingTool: null }),
    }),
    {
      name: "nsb-ui",
      partialize: (s) => ({
        sidebarCollapsed: s.sidebarCollapsed,
        lang: s.lang,
        serviceView: s.serviceView,
      }),
    }
  )
);


/** 订阅语言的翻译 hook：语言切换时所有使用处重渲染 */
export function useT() {
  const lang = useUI((s) => s.lang);
  return React.useCallback((key: Parameters<typeof translate>[1]) => translate(lang, key), [lang]);
}
