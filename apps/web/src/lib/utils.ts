import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import type { PackageView, ServiceStatus, StackItem } from "@nsb/schema";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** 与 sites::proxy_url 保持一致；返回实际配置使用的基础地址，非法输入返回 null。 */
export function normalizeProxyTarget(input: string): string | null {
  const target = input.trim();
  if (!target || /[\s\u0000-\u001f\u007f-\u009f"';{}$\\<>]/u.test(target)) return null;
  try {
    const address = target.includes("://") ? target : `http://${target}`;
    const url = new URL(address);
    if (!["http:", "https:"].includes(url.protocol) || !url.hostname || url.username || url.password
      || address.includes("?") || address.includes("#") || url.port === "0") return null;
    const normalized = url.toString();
    return normalized.endsWith("/") ? normalized : `${normalized}/`;
  } catch {
    return null;
  }
}

export const PHP_SITE_OPTIONS = [
  { key: "memory_limit", type: "size", initial: "512M" },
  { key: "upload_max_filesize", type: "size", initial: "64M" },
  { key: "post_max_size", type: "size", initial: "128M" },
  { key: "max_execution_time", type: "number", initial: "300" },
  { key: "max_input_time", type: "number", initial: "60" },
  { key: "max_input_vars", type: "number", initial: "1000" },
  { key: "max_file_uploads", type: "number", initial: "20" },
  { key: "display_errors", type: "switch", initial: "On" },
  { key: "log_errors", type: "switch", initial: "On" },
] as const;

/** 与 sites::validate_php_overrides 对齐；文件写入前后端仍会再次校验。 */
export function isPhpSiteSettingValid(key: string, value: string, previousValue?: string): boolean {
  const option = PHP_SITE_OPTIONS.find((option) => option.key === key);
  if (!value || /[\s\u0000-\u001f\u007f-\u009f]/u.test(value)) return false;
  if (!option) return value === previousValue && /^[a-zA-Z0-9_.]+$/.test(key) && !/['";$\[\]]/.test(value);
  if (option.type === "switch") return ["on", "off", "0", "1"].includes(value.toLowerCase());
  if (option.type === "size") {
    if (key === "memory_limit" && value === "-1") return true;
    const match = /^(\d{1,20})([kmg]?)$/i.exec(value);
    if (!match) return false;
    const multiplier = { "": 1n, k: 1024n, m: 1048576n, g: 1073741824n }[match[2].toLowerCase()]!;
    const bytes = BigInt(match[1]) * multiplier;
    return bytes <= 9223372036854775807n && (key !== "memory_limit" || bytes >= 2097152n);
  }
  if (key === "max_input_time" && value === "-1") return true;
  return /^\d+$/.test(value) && Number(value) <= 4294967295
    && (!["max_input_vars", "max_file_uploads"].includes(key) || Number(value) > 0);
}

/** 与后端证书校验一致：通配符只覆盖一个 DNS 标签，IP 和通配符站点要求精确匹配。 */
export function certificateCoversDomain(sans: string[], input: string): boolean {
  const domain = input.trim().replace(/\.+$/, "").toLowerCase();
  return sans.some((value) => {
    const san = value.toLowerCase();
    if (san === domain) return true;
    if (domain.startsWith("*.") || domain.includes(":") || /^\d+\.\d+\.\d+\.\d+$/.test(domain)) return false;
    const dot = domain.indexOf(".");
    return san.startsWith("*.") && dot > 0 && domain.slice(dot + 1) === san.slice(2);
  });
}

/** 与后端 stacks::resolve_service_id 一致：精确 ID 优先，固定版本缺失不替换。 */
export function resolveStackService(id: string, services: ServiceStatus[], packages: PackageView[]) {
  const exact = services.find((service) => service.id === id);
  if (exact || id.includes("@")) return exact;
  const installed = packages.filter((p) => p.id === id && p.install)
    .sort((a, b) => cmpVersionDesc(a.version, b.version));
  const active = installed.find((p) => p.active) ?? installed[0];
  return active ? services.find((service) => service.id === `${id}@${active.version}`) : undefined;
}

/** 同一实例被“跟随版本”和固定版本重复引用时只计一次；缺失项仍计入总数。 */
export function resolvedStackItems(items: StackItem[], services: ServiceStatus[], packages: PackageView[]) {
  const seen = new Set<string>();
  return [...items].sort((a, b) => a.order - b.order).flatMap((item) => {
    const service = resolveStackService(item.serviceId, services, packages);
    const key = service?.id ?? item.serviceId;
    if (seen.has(key)) return [];
    seen.add(key);
    return [{ item, service }];
  });
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
    .split("+")[0]
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

/* ---------- 平台兼容性（与 Rust 侧 install::current_os/arch 对齐） ---------- */

/** 粗粒度平台判定：OS 用 UA；架构尽力而为——
 *  Mac 默认按 arm64（2026 年 Apple Silicon 为主，Intel 机型 UA 无法区分），
 *  Windows 按 x64（WoA 上 x64 包可经仿真运行）。空数组 = 不限平台。 */
export function frontendPlatform(): { os: "windows" | "macos" | "linux"; arch: "x64" | "arm64" } {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  const os = /Win/i.test(ua) ? "windows" : /Mac/i.test(ua) ? "macos" : "linux";
  const arch: "x64" | "arm64" =
    os === "macos" && /Intel/.test(ua) ? "x64" : os === "macos" ? "arm64" : "x64";
  return { os, arch };
}

export function isPlatformCompatible(osList: string[] | undefined, archList: string[] | undefined): boolean {
  if (!osList?.length && !archList?.length) return true;
  const cur = frontendPlatform();
  const osOk = !osList?.length || osList.includes(cur.os);
  const archOk = !archList?.length || archList.includes(cur.arch);
  return osOk && archOk;
}
