"use client";

import * as React from "react";
import {
  ACCENT_PRESETS,
  MONO_FONT_OPTIONS,
  UI_FONT_OPTIONS,
  type AppSettings,
} from "@nsb/schema";

/* ============================================================
   外观应用层：把设置里的外观项写到 :root / <html> 上。
   所有「读设置 → 改样式」都只走这里，避免每个页面各写一份
   style.setProperty 导致状态互相覆盖。
   ============================================================ */

/** #RRGGBB / #RGB → {r,g,b}；解析失败返回 null */
export function parseHex(hex: string): { r: number; g: number; b: number } | null {
  const s = hex.trim().replace(/^#/, "");
  if (!/^[0-9a-fA-F]{3}$|^[0-9a-fA-F]{6}$/.test(s)) return null;
  const full = s.length === 3 ? s.split("").map((c) => c + c).join("") : s;
  return {
    r: parseInt(full.slice(0, 2), 16),
    g: parseInt(full.slice(2, 4), 16),
    b: parseInt(full.slice(4, 6), 16),
  };
}

/** 自定义色 → hsl 字符串（用于派生 hover / soft 等半透明变体） */
export function hexToHsl(hex: string): { h: number; s: number; l: number } | null {
  const rgb = parseHex(hex);
  if (!rgb) return null;
  const r = rgb.r / 255;
  const g = rgb.g / 255;
  const b = rgb.b / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  let h = 0;
  let s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
    else if (max === g) h = ((b - r) / d + 2) / 6;
    else h = ((r - g) / d + 4) / 6;
  }
  return { h: Math.round(h * 360), s: Math.round(s * 100), l: Math.round(l * 100) };
}

export function hslToHex(h: number, s: number, l: number): string {
  const sn = s / 100;
  const ln = l / 100;
  const k = (n: number) => (n + h / 30) % 12;
  const a = sn * Math.min(ln, 1 - ln);
  const f = (n: number) => ln - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  const to = (v: number) =>
    Math.round(255 * v)
      .toString(16)
      .padStart(2, "0");
  return `#${to(f(0))}${to(f(8))}${to(f(4))}`;
}

/** 主题色的「预设色卡」颜色（与设置页色卡显示一致） */
export function presetSwatch(hue: number, dark: boolean) {
  if (Math.abs(hue - 211) <= 2) return dark ? "#0091FF" : "#0088FF";
  return `hsl(${hue} 84% ${dark ? 58 : 48}%)`;
}

/** 当前主题色对应的 hue（自定义颜色会被换算成 hue，供 ::selection 等使用） */
export function effectiveHue(settings: Pick<AppSettings, "accentHex" | "accentHue">): number {
  if (settings.accentHex) {
    const hsl = hexToHsl(settings.accentHex);
    if (hsl) return hsl.h;
  }
  return settings.accentHue;
}

/** 本机字体 id 前缀：`local:<字体族名>`，字体名来自系统扫描或用户手填 */
export const LOCAL_FONT_PREFIX = "local:";

export function isLocalFontId(id: string): boolean {
  return id.startsWith(LOCAL_FONT_PREFIX) && id.slice(LOCAL_FONT_PREFIX.length).trim().length > 0;
}

export function localFontFamily(id: string): string {
  return id.slice(LOCAL_FONT_PREFIX.length).trim().replace(/"/g, "");
}

export function uiFontStack(id: string): string {
  if (isLocalFontId(id)) {
    return `"${localFontFamily(id)}", -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif`;
  }
  return UI_FONT_OPTIONS.find((f) => f.id === id)?.stack ?? UI_FONT_OPTIONS[0].stack;
}

export function monoFontStack(id: string): string {
  if (isLocalFontId(id)) {
    return `"${localFontFamily(id)}", ui-monospace, Consolas, "Courier New", monospace`;
  }
  return MONO_FONT_OPTIONS.find((f) => f.id === id)?.stack ?? MONO_FONT_OPTIONS[0].stack;
}

/**
 * 主题色 → 实心填充色（主按钮/选中态用），浅色与深色各一组。
 * 预设色：按 hue 生成；自定义色：浅色下保持用户亮度，深色下抬亮以保证对比度。
 */
export function accentSolids(s: Pick<AppSettings, "accentHex" | "accentHue">) {
  if (s.accentHex) {
    const hsl = hexToHsl(s.accentHex);
    if (hsl) {
      const l = Math.min(94, Math.max(8, hsl.l));
      const dk = Math.max(l, 56); // 深色底上太暗会看不清
      return {
        light: `hsl(${hsl.h} ${hsl.s}% ${l}%)`,
        lightHover: `hsl(${hsl.h} ${hsl.s}% ${Math.min(94, l + 7)}%)`,
        dark: `hsl(${hsl.h} ${hsl.s}% ${dk}%)`,
        darkHover: `hsl(${hsl.h} ${hsl.s}% ${Math.min(94, dk + 6)}%)`,
      };
    }
  }
  const h = s.accentHue;
  /* systemBlue 档（蔚蓝预设）：浅色 #0088FF / 深色 #0091FF，与 Apple 语义色完全一致 */
  if (Math.abs(h - 211) <= 2) {
    return {
      light: "hsl(211 100% 50%)",
      lightHover: "hsl(211 100% 54%)",
      dark: "hsl(211 100% 50%)",
      darkHover: "hsl(211 100% 56%)",
    };
  }
  /* 其余预设：高饱和 + 中亮度，贴近 Apple 强调色的鲜艳度但不刺眼 */
  return {
    light: `hsl(${h} 84% 48%)`,
    lightHover: `hsl(${h} 86% 54%)`,
    dark: `hsl(${h} 90% 58%)`,
    darkHover: `hsl(${h} 92% 64%)`,
  };
}

/**
 * 把外观设置写到 DOM。桌面端由启动同步 + 设置页调用；浏览器（dev）同样生效。
 * 幂等：重复调用只是重写同一组变量。
 *
 * 注意：这里只写「原料」变量（色相 / 浅色组 / 深色组），
 * 具体 --primary 交给 CSS 按 .dark 选择 —— 因为内联样式优先级高于类选择器，
 * 直接写 --primary 会导致深色模式失效。
 */
export function applyAppearance(s: Partial<AppSettings>) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;

  /* 主题色：自定义颜色优先。这里同时写实心色，
     否则设置里选的强调色只会影响 ::selection，看起来像没生效。 */
  const accent = { accentHex: s.accentHex ?? "", accentHue: s.accentHue ?? 211 };
  const hue = effectiveHue(accent);
  root.style.setProperty("--accent-h", String(hue));
  const custom = s.accentHex ? parseHex(s.accentHex) : null;
  root.style.setProperty("--accent-hex", custom && s.accentHex ? s.accentHex : "transparent");
  const solid = accentSolids(accent);
  root.style.setProperty("--accent-solid-light", solid.light);
  root.style.setProperty("--accent-solid-light-hover", solid.lightHover);
  root.style.setProperty("--accent-solid-dark", solid.dark);
  root.style.setProperty("--accent-solid-dark-hover", solid.darkHover);
  /* 辉光同样写成浅/深两组，由 CSS 按 .dark 取用 —— 只写一个 --accent-glow
     会以内联样式压过 .dark 里的覆盖，深色模式拿到的是浅色辉光。 */
  root.style.setProperty("--accent-glow-light", `hsl(${hue} 70% 45% / 0.28)`);
  root.style.setProperty("--accent-glow-dark", `hsl(${hue} 70% 50% / 0.34)`);

  /* 字体与字号 */
  root.style.setProperty("--app-font-sans", uiFontStack(s.uiFont ?? "sf"));
  root.style.setProperty("--app-font-mono", monoFontStack(s.codeFont ?? "sf-mono"));
  const size = Math.min(18, Math.max(10, s.codeFontSize ?? 11.5));
  root.style.setProperty("--code-font-size", `${size}px`);
  const scale = Math.min(1.25, Math.max(0.85, s.uiScale ?? 1));
  root.style.setProperty("--app-font-scale", String(scale));
  root.style.setProperty("--app-zoom", String(scale));

  /* 减少动态 */
  root.classList.toggle("reduce-motion", !!s.reduceMotion);

  /* 滚动条：默认隐藏（内容照常滚动） */
  root.classList.toggle("nsb-hide-scrollbars", s.hideScrollbars !== false);
}

/** 设置页里给色卡用：把自定义颜色转成可显示的 hsl 描述 */
export function describeAccent(s: Pick<AppSettings, "accentHex" | "accentHue">) {
  if (s.accentHex) {
    const hsl = hexToHsl(s.accentHex);
    if (hsl) return `hsl(${hsl.h} ${hsl.s}% ${hsl.l}%)`;
  }
  return `hsl(${s.accentHue} 82% 55%)`;
}

/** 预设 → 是否命中（hue 允许 ±2 误差） */
export function matchingPreset(s: Pick<AppSettings, "accentHex" | "accentHue">): string | null {
  if (s.accentHex) return null;
  const p = ACCENT_PRESETS.find((x) => Math.abs(x.hue - s.accentHue) <= 2);
  return p?.id ?? null;
}

/** 外观设置变化时重放一次（用于非设置页的全局同步） */
export function useAppearanceSync(settings: Partial<AppSettings> | undefined) {
  React.useEffect(() => {
    if (settings) applyAppearance(settings);
  }, [
    settings?.accentHex,
    settings?.accentHue,
    settings?.uiFont,
    settings?.codeFont,
    settings?.codeFontSize,
    settings?.uiScale,
    settings?.reduceMotion,
    settings?.hideScrollbars,
  ]);
}
