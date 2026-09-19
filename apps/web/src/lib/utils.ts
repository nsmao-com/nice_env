import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function fmtBytes(n: number, digits = 1): string {
  if (!n || n < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / Math.pow(1024, i)).toFixed(i === 0 ? 0 : digits)} ${units[i]}`;
}

export function fmtSpeed(bps: number): string {
  return `${fmtBytes(bps, 1)}/s`;
}

export function fmtDuration(sec: number): string {
  if (sec < 0 || !isFinite(sec)) return "--";
  if (sec < 60) return `${Math.round(sec)}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m${Math.round(sec % 60)}s`;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return `${h}h${m}m`;
}

export function fmtUptime(sec: number): string {
  if (sec < 60) return `${Math.floor(sec)} 秒`;
  if (sec < 86400) return `${Math.floor(sec / 60)} 分钟`;
  return `${Math.floor(sec / 86400)} 天`;
}

export function timeAgo(ts: number): string {
  const d = Date.now() - ts;
  if (d < 60_000) return "刚刚";
  if (d < 3_600_000) return `${Math.floor(d / 60_000)} 分钟前`;
  if (d < 86_400_000) return `${Math.floor(d / 3_600_000)} 小时前`;
  return new Date(ts).toLocaleDateString("zh-CN");
}

/* ---------- 版本号比较 ---------- */

/** 预发布标记：这些版本的语义低于同号正式版 */
const PRERELEASE_MARKERS = ["rc", "beta", "alpha", "dev", "preview", "snapshot", "nightly"];

export function isPrerelease(v: string): boolean {
  const low = v.toLowerCase();
  return PRERELEASE_MARKERS.some((m) => low.includes(m));
}

/** 版本号 → 数字段。忽略 v/V 前缀（清单里 v1.19.1 与 1.19.0 并存），
 *  段内取前导数字（1.2.3rc1 → [1,2,3,1]）。与 Rust 侧 cmp_version_desc 对齐。 */
export function versionParts(v: string): number[] {
  return v
    .replace(/^[vV]/, "")
    .split(/[.\-_+]/)
    .map((s) => {
      const m = s.match(/^\d+/);
      return m ? parseInt(m[0], 10) : 0;
    });
}

/** 版本号降序比较：数值逐段比较，正式版优先于预发布版。
 *  统一出口，避免各处各写一份（之前就有两处重复且都漏了 v 前缀）。 */
export function cmpVersionDesc(a: string, b: string): number {
  const pa = versionParts(a);
  const pb = versionParts(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pb[i] ?? 0) - (pa[i] ?? 0);
    if (d !== 0) return d;
  }
  const ra = isPrerelease(a) ? 1 : 0;
  const rb = isPrerelease(b) ? 1 : 0;
  if (ra !== rb) return ra - rb; // 正式版（0）排前面
  return b.localeCompare(a);
}
