/* ============================================================
   本机字体枚举。
   两条路：
   1. Local Font Access（queryLocalFonts）—— 列表完整，但 WebView2 / 部分
      浏览器里没权限，直接被拒；
   2. canvas 度量探测 —— 零权限、任何环境可用：给候选字体族名排一段
      混合中英文文本，与基线字体（monospace/sans-serif/serif）比宽度，
      宽度不同说明系统里真的装了这个字体。
   设置页进页面就用 (2) 给出基础列表（不用用户点按钮），
   点「扫描本机字体」时 (1)+(2) 合并去重。
   随应用打包的 webfont（IBM Plex / Inter / JetBrains Mono / Fira Code）
   会被 canvas 探测误判为「本机已装」，从结果里排除 —— 它们本来就在预设里。
   ============================================================ */

/** 随应用打包的 webfont 族名（含 @fontsource 的 *Variable 变体），不作本机字体上报 */
const BUNDLED_FAMILIES = new Set([
  "IBM Plex Sans",
  "IBM Plex Sans Variable",
  "IBM Plex Mono",
  "JetBrains Mono",
  "JetBrains Mono Variable",
  "Inter",
  "Inter Variable",
  "Fira Code",
  "Fira Code Variable",
]);

/** 常见本机字体候选：Windows 自带 / macOS 自带 / 常见中文与编程字体 */
const CANDIDATE_FAMILIES = [
  // Windows 核心
  "Arial", "Arial Black", "Arial Narrow", "Bahnschrift", "Calibri", "Candara",
  "Cambria", "Comic Sans MS", "Consolas", "Constantia", "Corbel", "Courier New",
  "Ebrima", "Franklin Gothic Medium", "Gabriola", "Gadugi", "Georgia", "Impact",
  "Lucida Console", "Lucida Sans Unicode", "Malgun Gothic", "Microsoft Himalaya",
  "Microsoft JhengHei", "Microsoft New Tai Lue", "Microsoft PhagsPa",
  "Microsoft Sans Serif", "Microsoft Tai Le", "Microsoft YaHei", "Microsoft YaHei UI",
  "MingLiU", "MingLiU-ExtB", "Mongolian Baiti", "MS Gothic", "MS Mincho", "MV Boli",
  "Myanmar Text", "Nirmala UI", "Palatino Linotype", "Segoe Print", "Segoe Script",
  "Segoe UI", "Segoe UI Emoji", "Segoe UI Historic", "Segoe UI Symbol", "SimHei",
  "SimSun", "SimSun-ExtB", "Sitka Text", "Sylfaen", "Symbol", "Tahoma",
  "Times New Roman", "Trebuchet MS", "Verdana", "Webdings", "Wingdings",
  "Yu Gothic", "Yu Gothic UI",
  // Windows 中文（本地化名）
  "微软雅黑", "新宋体", "宋体", "黑体", "楷体", "仿宋", "等线",
  // macOS 核心
  "American Typewriter", "Andale Mono", "Arial Rounded MT Bold", "Avenir",
  "Avenir Next", "Baskerville", "Chalkboard", "Chalkduster", "Cochin",
  "Copperplate", "Courier", "Didot", "Futura", "Geneva", "Gill Sans",
  "Helvetica", "Helvetica Neue", "Hiragino Maru Gothic ProN", "Hiragino Sans",
  "Hiragino Sans GB", "Hoefler Text", "Marker Felt", "Menlo", "Monaco",
  "Noteworthy", "Optima", "Papyrus", "PingFang SC", "Rockwell", "SF Pro",
  "SF Pro Display", "SF Pro Text", "Skia", "Snell Roundhand", "Times", "Zapfino",
  "苹方-简", "宋体-简", "华文楷体", "华文宋体", "华文黑体", "华文仿宋",
  // 常见编程 / 开源字体
  "Cascadia Code", "Cascadia Mono", "Source Code Pro", "Roboto Mono",
  "Ubuntu Mono", "Inconsolata", "Hack", "Iosevka", "Sarasa Mono SC", "更纱黑体",
  "Maple Mono", "MesloLGS NF", "Cousine", "Anonymous Pro", "Overpass Mono",
  // 常见开源正文 / 中文
  "Roboto", "Noto Sans", "Noto Sans SC", "Noto Serif SC", "Noto Sans CJK SC",
  "Source Han Sans SC", "思源黑体", "思源宋体", "WenQuanYi Micro Hei", "文泉驿微米黑",
];

let canvasCache: string[] | null = null;

/** canvas 度量探测（零权限）。结果按模块缓存，进页面只算一次。 */
export function detectLocalFonts(): string[] {
  if (typeof document === "undefined") return [];
  if (canvasCache) return canvasCache;
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  if (!ctx) return [];

  const TEXT = "mmmwwwiiilll0123 中文『字体』度量 @#";
  const BASES = ["monospace", "sans-serif", "serif"] as const;
  const baseWidth = new Map<string, number>();
  for (const b of BASES) {
    ctx.font = `72px ${b}`;
    baseWidth.set(b, ctx.measureText(TEXT).width);
  }

  const found: string[] = [];
  for (const fam of CANDIDATE_FAMILIES) {
    for (const b of BASES) {
      ctx.font = `72px "${fam}", ${b}`;
      if (Math.abs(ctx.measureText(TEXT).width - (baseWidth.get(b) ?? 0)) > 0.5) {
        found.push(fam);
        break;
      }
    }
  }
  canvasCache = found.filter((f) => !BUNDLED_FAMILIES.has(f));
  return canvasCache;
}

/** Local Font Access 完整枚举；API 不存在或权限被拒时抛错/返回空。 */
export async function listLocalFontsNative(): Promise<string[]> {
  const q = (window as unknown as {
    queryLocalFonts?: () => Promise<Array<{ family: string; fullName: string }>>;
  }).queryLocalFonts;
  if (!q) throw new Error("Local Font Access unavailable");
  const fonts = await q.call(window);
  return Array.from(new Set(fonts.map((f) => f.family)));
}

/** 扫描 = 原生枚举（可用时）+ canvas 探测，合并去重。 */
export async function scanLocalFonts(): Promise<{ families: string[]; native: boolean }> {
  let native: string[] = [];
  try {
    native = await listLocalFontsNative();
  } catch {
    /* 权限被拒或 API 不在：走 canvas 探测 */
  }
  const families = Array.from(new Set([...native, ...detectLocalFonts()]))
    .filter((f) => f.trim() && !BUNDLED_FAMILIES.has(f))
    .sort((a, b) => a.localeCompare(b));
  return { families, native: native.length > 0 };
}
