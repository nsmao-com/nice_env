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
