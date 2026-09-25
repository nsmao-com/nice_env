//! 项目扫描：指一个目录，识别出它是什么项目，并给出建站建议。
//!
//! 这是 FlyEnv / ServBay / phpStudy 都没做顺的一步：用户手上已经有一堆项目
//! 目录了，却还要在「新建站点」表单里手工选 kind、填 root、挑 PHP 版本、
//! 选伪静态。这个模块负责把「看一眼目录就知道该怎么配」这件事做掉。
//!
//! 识别依据按「可信度从高到低」排序，取第一个命中的：
//! 1. 明确的框架标记文件（artisan / wp-config.php / next.config.* / go.mod …）
//! 2. 依赖清单里的框架依赖（composer.json 的 require、package.json 的 deps）
//! 3. 入口文件形态（public/index.php / index.php / index.html）
//! 4. 兜底：目录里有没有 php / html / js 文件
//!
//! 只做**只读扫描**：不修改、不写入、不执行项目里的任何东西。
//! 扫描结果交给用户确认后才用于建站。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// 识别出的项目类型。与 SiteKind 对齐，但更细——同一个 SiteKind 下
/// 不同框架的伪静态规则与文档根目录都不一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    Laravel,
    ThinkPhp,
    WordPress,
    Symfony,
    CodeIgniter,
    GenericPhp,
    StaticHtml,
    NextJs,
    Vite,
    NuxtJs,
    NodeGeneric,
    Python,
    Go,
    Java,
    Unknown,
}

impl ProjectKind {
    /// 映射到站点类型
    pub fn site_kind(&self) -> &'static str {
        match self {
            ProjectKind::Laravel
            | ProjectKind::ThinkPhp
            | ProjectKind::WordPress
            | ProjectKind::Symfony
            | ProjectKind::CodeIgniter
            | ProjectKind::GenericPhp => "php",
            ProjectKind::StaticHtml => "static",
            ProjectKind::NextJs
            | ProjectKind::Vite
            | ProjectKind::NuxtJs
            | ProjectKind::NodeGeneric => "node",
            ProjectKind::Python => "python",
            ProjectKind::Go => "go",
            ProjectKind::Java => "java",
            ProjectKind::Unknown => "static",
        }
    }

    /// 建议的伪静态预设（与 Site 的 rewrite 字段对齐）
    pub fn rewrite(&self) -> &'static str {
        match self {
            ProjectKind::Laravel | ProjectKind::Symfony => "laravel",
            ProjectKind::ThinkPhp => "thinkphp",
            ProjectKind::WordPress => "wordpress",
            ProjectKind::NextJs
            | ProjectKind::NuxtJs
            | ProjectKind::Vite
            | ProjectKind::NodeGeneric => "spa-fallback",
            _ => "none",
        }
    }

    /// 文档根目录（相对项目根）。返回 None 表示就用项目根。
    pub fn document_root(&self) -> Option<&'static str> {
        match self {
            // Laravel / Symfony / ThinkPHP 5+ 的入口都在 public/
            ProjectKind::Laravel | ProjectKind::Symfony => Some("public"),
            ProjectKind::ThinkPhp => Some("public"),
            // Next.js 静态导出产物
            ProjectKind::NextJs => Some("out"),
            ProjectKind::Vite => Some("dist"),
            ProjectKind::NuxtJs => Some(".output/public"),
            _ => None,
        }
    }

    /// 这一类的「怎么跑起来」提示，直接显示给用户
    pub fn run_hint(&self) -> &'static str {
        match self {
            ProjectKind::Laravel => "需要 PHP + Composer；首次运行前执行 composer install",
            ProjectKind::ThinkPhp => "需要 PHP；入口在 public/index.php",
            ProjectKind::WordPress => "需要 PHP + MySQL；把 wp-config.php 里的库信息对上本站绑定的数据库",
            ProjectKind::Symfony => "需要 PHP + Composer；入口在 public/index.php",
            ProjectKind::CodeIgniter => "需要 PHP；入口在 public/index.php",
            ProjectKind::GenericPhp => "需要 PHP",
            ProjectKind::StaticHtml => "纯静态，无需运行时",
            ProjectKind::NextJs => "需要 Node；开发用 npm run dev（本应用可按 Node 站点代理），静态导出用 npm run build + out 目录",
            ProjectKind::Vite => "需要 Node；开发用 npm run dev（代理），构建产物在 dist",
            ProjectKind::NuxtJs => "需要 Node；开发用 npm run dev（代理），构建产物在 .output/public",
            ProjectKind::NodeGeneric => "需要 Node；建议用「反向代理」模式指向 npm run dev 的端口",
            ProjectKind::Python => "需要 Python；建议用「反向代理」模式指向 dev server 端口",
            ProjectKind::Go => "需要 Go；建议先用 go run 起服务，再用反向代理指向它",
            ProjectKind::Java => "需要 JDK；建议用反向代理指向应用端口",
            ProjectKind::Unknown => "未能识别为已知项目类型，默认按静态站点处理，可手动调整",
        }
    }

    /// 是否需要先跑前端的 dev server（前端类项目）
    pub fn needs_dev_server(&self) -> bool {
        matches!(
            self,
            ProjectKind::NextJs
                | ProjectKind::Vite
                | ProjectKind::NuxtJs
                | ProjectKind::NodeGeneric
                | ProjectKind::Python
                | ProjectKind::Go
                | ProjectKind::Java
        )
    }
}

/// 一个被识别出来的项目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedProject {
    /// 项目根目录（绝对路径）
    pub path: String,
    pub name: String,
    pub kind: ProjectKind,
    /// 建议的文档根（已拼成绝对路径）
    pub document_root: String,
    /// 站点类型 / 伪静态预设，直接可填进建站表单
    pub site_kind: String,
    pub rewrite: String,
    /// 该项目可用的 PHP 版本（Laravel 等对 PHP 版本有下限要求）
    pub php_min_version: Option<String>,
    /// 识别依据（说明为什么判成这个类型），用户可据此判断识别对不对
    pub evidence: Vec<String>,
    pub run_hint: String,
    pub needs_dev_server: bool,
    /// 建议的站点名（取目录名，做域名安全化）
    pub suggested_domain: String,
    /// 该项目是否已在本应用里建过站（按文档根匹配）
    pub already_configured: bool,
}

/// 扫描一个父目录，找出其中的项目（一层 + 常见子目录）。
///
/// 只扫一层深度是有意的：用户给的「工作目录」下面通常就是一个个项目文件夹，
/// 递归下去会把 vendor/node_modules 里的东西也当成项目，噪音远大于收益。
pub fn scan_dir(
    paths: &crate::paths::Paths,
    store: &crate::store::Store,
    root: &Path,
) -> Result<Vec<ScannedProject>> {
    if !root.is_dir() {
        return Err(AppError::new("NOT_A_DIR", "指定的路径不是目录"));
    }
    // 已配置站点的文档根集合，用于标记「已建过站」
    let configured: Vec<String> = crate::sites::list(store)
        .unwrap_or_default()
        .into_iter()
        .map(|s| normalize(&s.root_dir))
        .collect();

    let mut out: Vec<ScannedProject> = Vec::new();
    // 根目录自身也可能是个项目
    if let Some(p) = detect_one(root, &configured) {
        out.push(p);
    }
    let rd = std::fs::read_dir(root).map_err(|e| AppError::io("读取目录", e))?;
    for e in rd.flatten() {
        let child = e.path();
        if !child.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        // 跳过明显的非项目目录，省得列表里全是噪音
        if is_noise_dir(&name) {
            continue;
        }
        if let Some(p) = detect_one(&child, &configured) {
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = paths; // 保留参数以便后续需要按版本探测
    Ok(out)
}

fn is_noise_dir(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with('.')
        || matches!(
            n.as_str(),
            "node_modules"
                | "vendor"
                | "target"
                | "dist"
                | "build"
                | "__pycache__"
                | ".git"
                | "windows"
                | "program files"
                | "program files (x86)"
                | "appdata"
                | "$recycle.bin"
        )
}

fn normalize(p: &str) -> String {
    p.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// 判断单个目录是什么项目
pub fn detect_one(dir: &Path, configured: &[String]) -> Option<ScannedProject> {
    let has = |rel: &str| dir.join(rel).exists();
    let read = |rel: &str| {
        std::fs::read_to_string(dir.join(rel))
            .ok()
            .unwrap_or_default()
    };

    let mut evidence: Vec<String> = Vec::new();
    let mut kind = ProjectKind::Unknown;
    let mut php_min: Option<String> = None;

    // ---- 1) 框架标记文件 ----
    if has("artisan") {
        kind = ProjectKind::Laravel;
        evidence.push("存在 artisan（Laravel 命令行入口）".into());
    } else if has("wp-config.php") || has("wp-config-sample.php") {
        kind = ProjectKind::WordPress;
        evidence.push("存在 wp-config.php（WordPress 配置）".into());
    } else if has("think") && has("app") {
        kind = ProjectKind::ThinkPhp;
        evidence.push("存在 think 命令与 app 目录（ThinkPHP）".into());
    } else if has("bin/console") && has("symfony.lock") {
        kind = ProjectKind::Symfony;
        evidence.push("存在 bin/console 与 symfony.lock（Symfony）".into());
    } else if has("spark") {
        kind = ProjectKind::CodeIgniter;
        evidence.push("存在 spark（CodeIgniter 4）".into());
    } else if has("next.config.js") || has("next.config.ts") || has("next.config.mjs") {
        kind = ProjectKind::NextJs;
        evidence.push("存在 next.config.*（Next.js）".into());
    } else if has("nuxt.config.ts") || has("nuxt.config.js") {
        kind = ProjectKind::NuxtJs;
        evidence.push("存在 nuxt.config.*（Nuxt）".into());
    } else if has("vite.config.ts") || has("vite.config.js") {
        kind = ProjectKind::Vite;
        evidence.push("存在 vite.config.*（Vite）".into());
    } else if has("go.mod") {
        kind = ProjectKind::Go;
        evidence.push("存在 go.mod（Go 模块）".into());
    } else if has("pom.xml") || has("build.gradle") || has("build.gradle.kts") {
        kind = ProjectKind::Java;
        evidence.push("存在 Maven/Gradle 构建文件（Java）".into());
    } else if has("pyproject.toml") || has("requirements.txt") || has("manage.py") {
        kind = ProjectKind::Python;
        evidence.push("存在 pyproject.toml / requirements.txt（Python）".into());
    }

    // ---- 2) 依赖清单里的框架 ----
    if kind == ProjectKind::Unknown && has("composer.json") {
        let c = read("composer.json").to_ascii_lowercase();
        if c.contains("laravel/framework") {
            kind = ProjectKind::Laravel;
            evidence.push("composer.json 依赖 laravel/framework".into());
        } else if c.contains("symfony/framework-bundle") {
            kind = ProjectKind::Symfony;
            evidence.push("composer.json 依赖 symfony/framework-bundle".into());
        } else if c.contains("topthink/framework") {
            kind = ProjectKind::ThinkPhp;
            evidence.push("composer.json 依赖 topthink/framework".into());
        } else if c.contains("codeigniter4/framework") {
            kind = ProjectKind::CodeIgniter;
            evidence.push("composer.json 依赖 codeigniter4/framework".into());
        } else {
            kind = ProjectKind::GenericPhp;
            evidence.push("composer.json 存在但无已知框架依赖".into());
        }
    }
    if kind == ProjectKind::Unknown && has("package.json") {
        let p = read("package.json").to_ascii_lowercase();
        if p.contains("\"next\"") {
            kind = ProjectKind::NextJs;
            evidence.push("package.json 依赖 next".into());
        } else if p.contains("\"nuxt\"") {
            kind = ProjectKind::NuxtJs;
            evidence.push("package.json 依赖 nuxt".into());
        } else if p.contains("\"vite\"") {
            kind = ProjectKind::Vite;
            evidence.push("package.json 依赖 vite".into());
        } else {
            kind = ProjectKind::NodeGeneric;
            evidence.push("package.json 存在但无已知框架依赖".into());
        }
    }

    // ---- 3) Laravel 等对 PHP 版本有下限：从 composer.json 的 require 里读 ----
    if matches!(kind, ProjectKind::Laravel | ProjectKind::Symfony) && has("composer.json") {
        let c = read("composer.json");
        if let Some(v) = extract_php_requirement(&c) {
            evidence.push(format!("composer.json 要求 php {v}"));
            php_min = Some(v);
        }
    }

    // ---- 4) 入口文件形态兜底 ----
    if kind == ProjectKind::Unknown {
        if has("public/index.php") {
            kind = ProjectKind::GenericPhp;
            evidence.push("存在 public/index.php".into());
        } else if has("index.php") {
            kind = ProjectKind::GenericPhp;
            evidence.push("存在 index.php".into());
        } else if has("index.html") || has("index.htm") {
            kind = ProjectKind::StaticHtml;
            evidence.push("存在 index.html".into());
        }
    }

    // 什么特征都没有 → 不算项目（避免把空目录 / 资源目录列进来）
    if kind == ProjectKind::Unknown {
        let has_any_code = ["php", "html", "htm"]
            .iter()
            .any(|ext| dir_has_extension(dir, ext));
        if !has_any_code {
            return None;
        }
        evidence.push("按目录内的文件类型兜底判断".into());
        // 有 php 文件就当 php，有 html 就当静态
        kind = if dir_has_extension(dir, "php") {
            ProjectKind::GenericPhp
        } else {
            ProjectKind::StaticHtml
        };
    }

    let name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.to_string_lossy().to_string());

    let doc_root = match kind.document_root() {
        Some(sub) => dir.join(sub),
        None => dir.to_path_buf(),
    };
    // 文档根不存在时退回项目根，免得建出一个指向空目录的站点
    let doc_root = if doc_root.is_dir() {
        doc_root
    } else {
        dir.to_path_buf()
    };

    let already = configured
        .iter()
        .any(|c| *c == normalize(&doc_root.to_string_lossy()));

    Some(ScannedProject {
        path: dir.to_string_lossy().to_string(),
        name: name.clone(),
        kind,
        document_root: doc_root.to_string_lossy().to_string(),
        site_kind: kind.site_kind().to_string(),
        rewrite: kind.rewrite().to_string(),
        php_min_version: php_min,
        evidence,
        run_hint: kind.run_hint().to_string(),
        needs_dev_server: kind.needs_dev_server(),
        suggested_domain: domain_from_name(&name),
        already_configured: already,
    })
}

/// 从目录名推一个安全的测试域名：小写、只留字母数字与连字符
pub fn domain_from_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // 连续连字符合并
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s = s.trim_matches('-').to_string();
    if s.is_empty() {
        s = "site".to_string();
    }
    // 域名不能以数字开头时也允许（.test 是内网域名），但要去掉超长
    s.truncate(40);
    format!("{s}.test")
}

/// 从 composer.json 里读 `require.php` 的版本约束
pub fn extract_php_requirement(composer_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(composer_json).ok()?;
    let req = v.get("require")?.as_object()?;
    for (k, val) in req {
        if k.eq_ignore_ascii_case("php") {
            return val.as_str().map(|s| s.to_string());
        }
    }
    None
}

fn dir_has_extension(dir: &Path, ext: &str) -> bool {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return false,
    };
    rd.flatten()
        .take(200)
        .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("nsb-scan-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn file(&self, rel: &str, content: &str) -> &Self {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, content).unwrap();
            self
        }
        fn dir(&self, rel: &str) -> &Self {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
            self
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn detect(t: &Tmp) -> Option<ScannedProject> {
        detect_one(&t.0, &[])
    }

    #[test]
    fn detects_laravel_by_artisan() {
        let t = Tmp::new("laravel");
        t.file("artisan", "").dir("public");
        t.file("public/index.php", "<?php");
        let p = detect(&t).expect("应识别为项目");
        assert_eq!(p.kind, ProjectKind::Laravel);
        assert_eq!(p.site_kind, "php");
        assert_eq!(p.rewrite, "laravel");
        assert!(p.document_root.replace('\\', "/").ends_with("/public"));
        assert!(p.evidence.iter().any(|e| e.contains("artisan")));
    }

    #[test]
    fn detects_wordpress() {
        let t = Tmp::new("wp");
        t.file("wp-config.php", "<?php").file("index.php", "<?php");
        let p = detect(&t).unwrap();
        assert_eq!(p.kind, ProjectKind::WordPress);
        assert_eq!(p.rewrite, "wordpress");
    }

    #[test]
    fn detects_nextjs_and_suggests_spa_fallback() {
        let t = Tmp::new("next");
        t.file("next.config.js", "module.exports={}");
        t.dir("out");
        let p = detect(&t).unwrap();
        assert_eq!(p.kind, ProjectKind::NextJs);
        assert_eq!(p.rewrite, "spa-fallback");
        assert!(p.needs_dev_server);
        assert!(p.document_root.replace('\\', "/").ends_with("/out"));
    }

    #[test]
    fn detects_go_project() {
        let t = Tmp::new("go");
        t.file("go.mod", "module x\ngo 1.22\n");
        let p = detect(&t).unwrap();
        assert_eq!(p.kind, ProjectKind::Go);
        assert!(p.needs_dev_server);
    }

    #[test]
    fn detects_framework_from_composer_deps() {
        // 没有 artisan，但 composer.json 里有 laravel/framework
        let t = Tmp::new("laraveldep");
        t.file(
            "composer.json",
            r#"{"require":{"php":">=8.1","laravel/framework":"^10.0"}}"#,
        );
        t.dir("public");
        t.file("public/index.php", "<?php");
        let p = detect(&t).unwrap();
        assert_eq!(p.kind, ProjectKind::Laravel);
        assert!(
            p.evidence.iter().any(|e| e.contains("laravel/framework")),
            "证据里应说明是靠 composer 依赖判断的：{:?}",
            p.evidence
        );
    }

    #[test]
    fn extracts_php_requirement() {
        assert_eq!(
            extract_php_requirement(r#"{"require":{"php":">=8.1"}}"#),
            Some(">=8.1".into())
        );
        assert_eq!(
            extract_php_requirement(r#"{"require":{"php":"^8.2"}}"#),
            Some("^8.2".into())
        );
        // 没有 php 约束
        assert_eq!(
            extract_php_requirement(r#"{"require":{"x/y":"1.0"}}"#),
            None
        );
        // 非法 JSON 不该 panic
        assert_eq!(extract_php_requirement("not json"), None);
        assert_eq!(extract_php_requirement(""), None);
    }

    #[test]
    fn laravel_records_php_requirement_as_evidence() {
        let t = Tmp::new("larapm");
        t.file("artisan", "");
        t.file("composer.json", r#"{"require":{"php":">=8.2"}}"#);
        t.dir("public");
        t.file("public/index.php", "<?php");
        let p = detect(&t).unwrap();
        assert_eq!(p.php_min_version.as_deref(), Some(">=8.2"));
    }

    #[test]
    fn static_html_detected() {
        let t = Tmp::new("static");
        t.file("index.html", "<h1>hi</h1>");
        let p = detect(&t).unwrap();
        assert_eq!(p.kind, ProjectKind::StaticHtml);
        assert_eq!(p.site_kind, "static");
        assert!(!p.needs_dev_server);
    }

    #[test]
    fn empty_dir_is_not_a_project() {
        let t = Tmp::new("empty");
        assert!(detect(&t).is_none(), "空目录不该被当成项目");
    }

    #[test]
    fn docs_only_dir_is_not_a_project() {
        let t = Tmp::new("docs");
        t.file("README.md", "# hi");
        assert!(detect(&t).is_none(), "只有文档的目录不该被当成项目");
    }

    #[test]
    fn generic_php_fallback() {
        let t = Tmp::new("phponly");
        t.file("index.php", "<?php echo 1;");
        let p = detect(&t).unwrap();
        assert_eq!(p.kind, ProjectKind::GenericPhp);
        assert_eq!(p.site_kind, "php");
    }

    #[test]
    fn document_root_falls_back_when_public_missing() {
        // 有 artisan 但没有 public/ —— 不该给出一个指向不存在目录的文档根
        let t = Tmp::new("nopublic");
        t.file("artisan", "");
        t.file("index.php", "<?php");
        let p = detect(&t).unwrap();
        assert_eq!(
            normalize(&p.document_root),
            normalize(&t.0.to_string_lossy()),
            "public 不存在时应退回项目根"
        );
    }

    #[test]
    fn domain_from_name_sanitizes() {
        assert_eq!(domain_from_name("My Shop"), "my-shop.test");
        assert_eq!(domain_from_name("my_shop"), "my-shop.test");
        assert_eq!(domain_from_name("中文项目"), "site.test");
        assert_eq!(domain_from_name("---"), "site.test");
        assert_eq!(domain_from_name("a"), "a.test");
    }

    #[test]
    fn domain_from_name_collapses_dashes_and_truncates() {
        assert_eq!(domain_from_name("a -- b"), "a-b.test");
        let long = domain_from_name(&"x".repeat(200));
        assert!(long.len() <= 46, "域名不该超长：{}", long.len());
    }

    #[test]
    fn scan_dir_skips_noise_directories() {
        let t = Tmp::new("scanroot");
        t.file("proj-a/index.php", "<?php");
        t.file("node_modules/some-pkg/index.js", "1");
        t.file("vendor/lib/index.php", "<?php");
        t.file(".git/config", "");
        let paths = crate::paths::Paths::new(t.0.clone());
        let store = crate::store::Store::open(t.0.join("test.sqlite")).unwrap();
        let found = scan_dir(&paths, &store, &t.0).unwrap();
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"proj-a"), "应找到 proj-a：{names:?}");
        assert!(
            !names
                .iter()
                .any(|n| *n == "node_modules" || *n == "vendor" || n.starts_with('.')),
            "不该把噪音目录当项目：{names:?}"
        );
    }

    #[test]
    fn scan_dir_marks_already_configured() {
        let t = Tmp::new("scanconf");
        t.file("proj-b/index.php", "<?php");
        let paths = crate::paths::Paths::new(t.0.clone());
        let store = crate::store::Store::open(t.0.join("test2.sqlite")).unwrap();
        let docroot = normalize(&t.0.join("proj-b").to_string_lossy());
        let found = scan_dir(&paths, &store, &t.0).unwrap();
        // 手工传已配置列表做断言（scan_dir 内部从 store 读，这里直接验证匹配逻辑）
        let again = detect_one(&t.0.join("proj-b"), &[docroot]);
        assert!(again.unwrap().already_configured);
        assert!(!found.is_empty());
    }

    #[test]
    fn scan_dir_rejects_non_directory() {
        let t = Tmp::new("notdir");
        let f = t.0.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let paths = crate::paths::Paths::new(t.0.clone());
        let store = crate::store::Store::open(t.0.join("test3.sqlite")).unwrap();
        assert!(scan_dir(&paths, &store, &f).is_err());
    }

    #[test]
    fn kind_mappings_cover_all_variants() {
        // 每个 kind 都必须给得出 site_kind，不能漏
        let all = [
            ProjectKind::Laravel,
            ProjectKind::ThinkPhp,
            ProjectKind::WordPress,
            ProjectKind::Symfony,
            ProjectKind::CodeIgniter,
            ProjectKind::GenericPhp,
            ProjectKind::StaticHtml,
            ProjectKind::NextJs,
            ProjectKind::Vite,
            ProjectKind::NuxtJs,
            ProjectKind::NodeGeneric,
            ProjectKind::Python,
            ProjectKind::Go,
            ProjectKind::Java,
            ProjectKind::Unknown,
        ];
        for k in all {
            assert!(!k.site_kind().is_empty(), "{k:?} 缺 site_kind");
            assert!(!k.run_hint().is_empty(), "{k:?} 缺 run_hint");
        }
    }
}
