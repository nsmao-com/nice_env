import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import type { PackageView, ServiceStatus, StackItem, SiteRuntime, BulkTarget, BulkReport } from "@nsb/schema";

export const APPLICATION_RUNTIMES = [
  { kind: "node", id: "node", label: "Node.js", args: ["server.js"] },
  { kind: "python", id: "python", label: "Python", args: ["app.py"] },
  { kind: "java", id: "temurin-jdk21", label: "Java", args: ["-jar", "app.jar"] },
  { kind: "go", id: "go", label: "Go", args: ["run", "."] },
] as const;

/** 地址格式与原生访问限制一致；网段规范化由后端统一完成。 */
export function validSiteAccessAddress(value: string): boolean {
  const parts = value.trim().split("/");
  if (!parts[0] || value.trim().length > 64 || parts.length > 2 || (parts.length === 2 && !/^\d+$/.test(parts[1]))) return false;
  const [host, mask] = parts;
  if (host.includes(":")) {
    if (!/^[\da-fA-F:.]+$/.test(host)) return false;
    try {
      const normalized = new URL(`http://[${host}]/`).hostname;
      const mapped = normalized.startsWith("[::ffff:");
      return mask === undefined || (Number(mask) <= 128 && (!mapped || Number(mask) >= 96));
    } catch { return false; }
  }
  return /^(?:0|[1-9]\d{0,2})(?:\.(?:0|[1-9]\d{0,2})){3}$/.test(host)
    && host.split(".").every((part) => Number(part) <= 255) && (mask === undefined || Number(mask) <= 32);
}

export function siteAccessProblem(access: SiteRuntime["access"]): "siteAccess.invalid" | "siteAccess.count" | null {
  if (!access) return null;
  if (!access.addresses.length || access.addresses.length > 32) return "siteAccess.count";
  return !["allow", "deny"].includes(access.mode) || access.addresses.some((value) => !validSiteAccessAddress(value)) ? "siteAccess.invalid" : null;
}

export function applicationRuntime(kind: SiteRuntime["kind"]) {
  return APPLICATION_RUNTIMES.find((runtime) => runtime.kind === kind);
}

/** 与原生应用配置校验一致；这里只接受规范化后的回环 IP。 */
export function validApplication(application: SiteRuntime["application"], proxyTarget: string) {
  if (!application) return true;
  const bytes = (value: string) => new TextEncoder().encode(value).length;
  if (!application.version || bytes(application.version) > 128 || /[\u0000-\u001f\u007f-\u009f]/.test(application.version)
    || !application.args.length || application.args.length > 64 || !application.args[0].trim()
    || application.args.some((arg) => arg.includes("\0") || bytes(arg) > 8192)
    || bytes(application.args.join("")) > 32 * 1024
    || (application.cwd && (bytes(application.cwd) > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(application.cwd)))) return false;
  try {
    const raw = proxyTarget.trim();
    const url = new URL(raw.includes("://") ? raw : `http://${raw}`);
    const ipv4 = /^127(?:\.\d{1,3}){3}$/.test(url.hostname) && url.hostname.split(".").every((part) => Number(part) <= 255);
    return url.protocol === "http:" && (ipv4 || url.hostname === "[::1]")
      && url.port !== "0" && !url.username && !url.password && !raw.includes("?") && !raw.includes("#") && url.pathname === "/";
  } catch { return false; }
}

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function bulkTarget(service: ServiceStatus): BulkTarget {
  return { id: service.id, version: service.version ?? null, label: service.label };
}

/** 重试保留原报告的成功项，避免一次失败重试使已完成的服务从结果中消失。 */
export function mergeBulkReport(previous: BulkReport, next: BulkReport): BulkReport {
  const retried = new Set(next.order);
  return { ...next, order: previous.order,
    succeeded: [...previous.succeeded.filter((id) => !retried.has(id)), ...next.succeeded],
    already: [...previous.already.filter((id) => !retried.has(id)), ...next.already],
    failed: [...previous.failed.filter((failure) => !retried.has(failure.serviceId)), ...next.failed],
  };
}

/** 与 envfile::is_secret_key 一致，新变量在落盘前也需要隐藏敏感值。 */
export function isEnvSecretKey(key: string): boolean {
  const value = key.toUpperCase();
  return /PASSWORD|PASSWD|SECRET|_KEY|TOKEN|PRIVATE|CREDENTIAL/.test(value)
    || value.endsWith("_PASS") || value.endsWith("KEY") || value === "DATABASE_URL";
}

export const ENV_FILE_PRESETS = [".env", ".env.local", ".env.development", ".env.development.local", ".env.production", ".env.production.local", ".env.test", ".env.test.local", ".env.dev", ".env.dev.local", ".env.prod", ".env.prod.local", ".env.staging", ".env.example", ".env.dist"];

export function isEnvFileName(name: string): boolean {
  return name === ".env" || (name.length <= 128 && /^\.env\.[A-Za-z0-9_.-]+$/.test(name) && !name.endsWith(".")
    && !/\.(nsb-backup|nsb-before-restore)$/i.test(name) && name.toLowerCase() !== ".env.local.php");
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

export function siteRedirectTarget(redirect: SiteRuntime["redirect"], domains: string[]): { url: string; error: null } | { url: null; error: "redirect.invalid" | "redirect.pathQuery" | "redirect.loop" } {
  const bad = { url: null, error: "redirect.invalid" } as const;
  if (!redirect || ![301, 302, 307, 308].includes(redirect.status)) return bad;
  const value = redirect.target.trim();
  if (!/^https?:\/\//i.test(value) || value.length > 8192 || /[\s\p{Cc}"'\\$;{}<>]/u.test(value)) return bad;
  try {
    const url = new URL(value);
    if (!["http:", "https:"].includes(url.protocol) || !url.hostname || url.username || url.password || url.port === "0") return bad;
    if (redirect.preservePath && (value.includes("?") || value.includes("#"))) return { url: null, error: "redirect.pathQuery" };
    const host = url.hostname.toLowerCase().replace(/\.$/, "");
    if (domains.some((domain) => { const name = domain.trim().toLowerCase(); return name.startsWith("*.") ? host.endsWith(name.slice(1)) : host === name; })) return { url: null, error: "redirect.loop" };
    return { url: url.href, error: null };
  } catch { return bad; }
}

export function proxyRulePath(value: string): string | null {
  const raw = value.trim(), path = raw.replace(/\/+$/, "");
  return path && path.length <= 256 && /^\/[A-Za-z0-9/._~-]+$/.test(raw) && !raw.includes("//")
    && !path.split("/").some((part) => part === "." || part === "..") ? path : null;
}

export function siteProxyProblem(runtime: SiteRuntime): "proxyRules.pathInvalid" | "proxyRules.targetInvalid" | "proxyRules.duplicate" | "proxyRules.limit" | "proxyRules.redirectInvalid" | null {
  const rules = runtime.proxyRules ?? [];
  if (rules.length > 16) return "proxyRules.limit";
  if (rules.length && runtime.kind === "redirect") return "proxyRules.redirectInvalid";
  const seen = new Set<string>();
  for (const rule of rules) {
    const path = proxyRulePath(rule.path);
    if (!path) return "proxyRules.pathInvalid";
    if (seen.has(path)) return "proxyRules.duplicate";
    seen.add(path);
    if (new TextEncoder().encode(rule.target).length > 8192 || !normalizeProxyTarget(rule.target)) return "proxyRules.targetInvalid";
  }
  return null;
}

export const SITE_ERROR_STATUSES = [400, 401, 403, 404, 405, 408, 429, 500, 502, 503, 504] as const;

/** 与 sites::normalize_error_pages 对齐，避免把非法 URL 路径提交给 Web 服务。 */
export function validSiteErrorPagePath(value: string): boolean {
  const path = value.trim();
  return path.length > 1 && path.length <= 512 && path.startsWith("/") && !path.includes("//")
    && !path.split("/").some((part) => part === "." || part === "..")
    && !/%(?:2e|2f|5c)/i.test(path)
    && !/%(?![0-9a-f]{2})/i.test(path)
    && /^[A-Za-z0-9/_.~!$&'()*+,;=:@%-]+$/.test(path);
}

export function siteErrorPagesProblem(runtime: SiteRuntime): "errorPages.invalid" | "errorPages.unsupported" | "errorPages.redirectInvalid" | null {
  const pages = runtime.errorPages ?? {};
  const keys = Object.keys(pages);
  if (!keys.length) return null;
  if (runtime.kind === "redirect") return "errorPages.redirectInvalid";
  if (keys.length > SITE_ERROR_STATUSES.length || keys.some((key) => !SITE_ERROR_STATUSES.includes(Number(key) as typeof SITE_ERROR_STATUSES[number]))) return "errorPages.unsupported";
  return Object.values(pages).some((path) => !validSiteErrorPagePath(path)) ? "errorPages.invalid" : null;
}

export function siteBasicAuthProblem(value: SiteRuntime["basicAuth"]): "basicAuth.usernameInvalid" | "basicAuth.passwordRequired" | "basicAuth.passwordInvalid" | null {
  if (!value?.enabled) return null;
  const username = value.username.trim();
  if (!username || new TextEncoder().encode(username).length > 128 || /[\s:\u0000-\u001f\u007f-\u009f]/u.test(username)) return "basicAuth.usernameInvalid";
  if (value.password !== undefined) {
    const length = new TextEncoder().encode(value.password).length;
    if (length < 8 || length > 72 || /[\u0000-\u001f\u007f-\u009f]/u.test(value.password)) return "basicAuth.passwordInvalid";
  } else if (!value.hasPassword) {
    return "basicAuth.passwordRequired";
  }
  return null;
}

export function proxyRuleExample(rule: NonNullable<SiteRuntime["proxyRules"]>[number]): string | null {
  const path = proxyRulePath(rule.path), target = normalizeProxyTarget(rule.target);
  return path && target ? `${target}${rule.stripPrefix ? "" : `${path.slice(1)}/`}users?limit=10` : null;
}

export const CORS_METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] as const;

export function corsOrigin(input: string): string | null {
  const raw = input.trim();
  if (raw === "*") return raw;
  if (!/^https?:\/\//i.test(raw) || new TextEncoder().encode(raw).length > 2048 || /[\s\p{Cc}\\"'$;{}<>]/u.test(raw)) return null;
  try {
    const url = new URL(raw);
    return url.hostname && !url.username && !url.password && url.port !== "0" && url.pathname === "/" && !raw.includes("?") && !raw.includes("#") ? url.origin : null;
  } catch { return null; }
}

export function siteCorsProblem(cors: SiteRuntime["cors"]): "cors.originInvalid" | "cors.wildcardInvalid" | "cors.methodsInvalid" | "cors.headersInvalid" | "cors.ageInvalid" | null {
  if (!cors) return null;
  if (!cors.origins.length || cors.origins.length > 32 || cors.origins.some((origin) => !corsOrigin(origin))) return "cors.originInvalid";
  const origins = new Set(cors.origins.map(corsOrigin));
  if (origins.has("*") && (cors.credentials || origins.size !== 1)) return "cors.wildcardInvalid";
  if (!cors.methods.length || cors.methods.length > CORS_METHODS.length || cors.methods.some((method) => !(CORS_METHODS as readonly string[]).includes(method))) return "cors.methodsInvalid";
  if ([cors.allowedHeaders, cors.exposedHeaders].some((values) => values.length > 64 || values.some((value) => !/^[A-Za-z0-9_-]{1,128}$/.test(value.trim())))) return "cors.headersInvalid";
  if (!Number.isInteger(cors.maxAge) || cors.maxAge < 0 || cors.maxAge > 86400) return "cors.ageInvalid";
  return null;
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

/** 与后端 stacks::resolve_items 一致；保留实际实例和版本约束，供执行前核对。 */
export function stackServiceTarget(id: string, services: ServiceStatus[], packages: PackageView[]) {
  const exact = services.find((service) => service.id === id);
  if (exact) return { service: exact, expectedVersion: exact.version };
  const separator = id.indexOf("@");
  if (separator !== -1) {
    const base = id.slice(0, separator), version = id.slice(separator + 1);
    const service = services.find((service) => service.id === base);
    return service && packages.some((p) => p.id === base && sameVersion(p.version, version) && p.install)
      ? { service, expectedVersion: version } : undefined;
  }
  const installed = packages.filter((p) => p.id === id && p.install)
    .sort((a, b) => cmpVersionDesc(a.version, b.version));
  const active = installed.find((p) => p.active) ?? installed[0];
  const service = active ? services.find((service) => service.id === `${id}@${active.version}`) : undefined;
  return service ? { service, expectedVersion: service.version } : undefined;
}

/** 状态只归属于符合版本约束的实例，不能把另一版本计为已运行。 */
export function resolveStackService(id: string, services: ServiceStatus[], packages: PackageView[]) {
  const target = stackServiceTarget(id, services, packages);
  return target && sameOptionalVersion(target.service.version, target.expectedVersion) ? target.service : undefined;
}

export function stackVersionConflicts(items: StackItem[], services: ServiceStatus[], packages: PackageView[]) {
  const versions = new Map<string, string | null | undefined>();
  const conflicts = new Set<string>();
  for (const item of items) {
    const target = stackServiceTarget(item.serviceId, services, packages);
    if (!target) continue;
    const { service, expectedVersion } = target;
    if (versions.has(service.id) && !sameOptionalVersion(versions.get(service.id), expectedVersion)) conflicts.add(service.id);
    versions.set(service.id, expectedVersion);
  }
  return [...conflicts];
}

/** 同一实例被“跟随版本”和固定版本重复引用时只计一次；缺失项仍计入总数。 */
export function resolvedStackItems(items: StackItem[], services: ServiceStatus[], packages: PackageView[]) {
  const seen = new Set<string>();
  return [...items].sort((a, b) => a.order - b.order).flatMap((item) => {
    const target = stackServiceTarget(item.serviceId, services, packages);
    const service = target && sameOptionalVersion(target.service.version, target.expectedVersion) ? target.service : undefined;
    const key = target ? JSON.stringify([target.service.id, target.expectedVersion]) : item.serviceId;
    if (seen.has(key)) return [];
    seen.add(key);
    return [{ item, service, target }];
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
  return prereleaseStart(v) !== undefined;
}

function prereleaseStart(v: string): number | undefined {
  const low = v.split("+")[0].toLowerCase();
  const positions = PRERELEASE_MARKERS.map((marker) => low.indexOf(marker)).filter((i) => i >= 0);
  return positions.length ? Math.min(...positions) : undefined;
}

/** 主版本数字段不包含预发布序号或构建号；与 Rust 侧保持一致。 */
export function versionParts(v: string): number[] {
  const normalized = v.replace(/^[vV]/, "").split("+")[0];
  return normalized.slice(0, prereleaseStart(normalized) ?? normalized.length)
    .split(/[.\-_+]/)
    .map((s) => {
      const m = s.match(/^\d+/);
      return m ? parseInt(m[0], 10) : 0;
    });
}

/** 统一上游 tag、清单和安装记录之间可能出现的 v 前缀。 */
export function normalizeVersion(version: string): string {
  return version.trim().replace(/^[vV]+/, "");
}

export function sameVersion(left: string | null | undefined, right: string | null | undefined): boolean {
  return left != null && right != null && normalizeVersion(left) === normalizeVersion(right);
}

/** 比较允许为空的服务版本；空值只有和另一个空值才算相同。 */
export function sameOptionalVersion(left: string | null | undefined, right: string | null | undefined): boolean {
  if (left == null || right == null) return left == null && right == null;
  return sameVersion(left, right);
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
  return naturalVersionCompare(b.replace(/^[vV]/, ""), a.replace(/^[vV]/, ""));
}

/** 构建号和预发布序号按数字比较，不依赖浏览器语言或浮点数精度。 */
function naturalVersionCompare(a: string, b: string): number {
  const aa = a.toLowerCase().match(/[0-9]+|[^0-9]+/g) ?? [];
  const bb = b.toLowerCase().match(/[0-9]+|[^0-9]+/g) ?? [];
  for (let i = 0; i < Math.max(aa.length, bb.length); i++) {
    let x = aa[i], y = bb[i];
    if (x === undefined || y === undefined) return Number(x !== undefined) - Number(y !== undefined);
    if (/^[0-9]/.test(x) && /^[0-9]/.test(y)) {
      x = x.replace(/^0+/, ""); y = y.replace(/^0+/, "");
      if (x.length !== y.length) return x.length - y.length;
    }
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
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

/** 保留仍存在的已保存顺序，新出现的项目按原顺序追加。 */
export function orderedDisplayIds(ids: string[], saved: string[]): string[] {
  const current = new Set(ids);
  return [...new Set([...saved.filter((id) => current.has(id)), ...ids])];
}

/** 筛选中排序只替换可见项目所在的位置，不移动隐藏项目。 */
export function reorderVisibleIds(order: string[], visible: string[], active: string, over: string): string[] {
  const from = visible.indexOf(active);
  const to = visible.indexOf(over);
  if (from < 0 || to < 0 || from === to) return order;
  const moved = [...visible];
  moved.splice(to, 0, moved.splice(from, 1)[0]);
  const selected = new Set(visible);
  let index = 0;
  return order.map((id) => selected.has(id) ? moved[index++] : id);
}

/** Hostname only: URLs and ports belong in separate fields. */
export function isSiteHostname(value: string): boolean {
  const host = value.trim().toLowerCase();
  if (host === "localhost") return true;
  if (/^[0-9.]+$/.test(host)) return host.split(".").length === 4 && host.split(".").every(part => /^(0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255);
  const name = host.replace(/^\*\./, "");
  return name.length <= 253 && name.includes(".") && name.split(".").every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label));
}
