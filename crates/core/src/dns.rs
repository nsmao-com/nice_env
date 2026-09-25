//! 本地域名解析（CoreDNS 编排）：把 `*.{tld}` 通配解析到 127.0.0.1，
//! 其余查询转发公共 DNS。对标 ServBay 的 dnsmasq / FlyEnv 的内置 DNS——
//! hosts 文件不支持通配符，DNS 才是 `.test` 域名的根治方案。
//!
//! 用户接入方式：把系统/网卡的 DNS 指到 127.0.0.1（标准档端口 53）。

use crate::error::Result;
use crate::paths::{write_with_backup, Paths};

/// 生成 Corefile：`{tld}` 区用 template 插件通配应答，其余转发公共 DNS。
/// 区名不带端口——coredns 以 `-dns.port` 统一指定监听端口。
pub fn render_corefile(tld: &str, upstreams: &[&str]) -> String {
    let tld = tld.trim().trim_start_matches('.').to_ascii_lowercase();
    let upstream = if upstreams.is_empty() {
        "8.8.8.8 1.1.1.1"
    } else {
        &upstreams.join(" ")
    };
    format!(
        r#"# NiceEnv managed Corefile — *.{tld} 通配解析到 127.0.0.1，其余转发公共 DNS
{tld} {{
    template IN ANY {tld} {{
        answer "{{{{ .Name }}}} 60 IN A 127.0.0.1"
    }}
    errors
}}
. {{
    forward . {upstream}
    errors
}}
"#,
        tld = tld,
        upstream = upstream,
    )
}

/// 写 Corefile（每次启动前重写：TLD/站点变化自动跟上）
pub fn write_corefile(paths: &Paths, tld: &str, upstreams: &[&str]) -> Result<()> {
    let dir = paths.etc().join("coredns");
    std::fs::create_dir_all(&dir)?;
    let conf = render_corefile(tld, upstreams);
    write_with_backup(&dir.join("Corefile"), &conf, &paths.backup())
        .map_err(|e| crate::error::AppError::io("写入 Corefile", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corefile_answers_wildcard_tld_and_forwards_rest() {
        let c = render_corefile("test", &[]);
        assert!(c.contains("test {"), "应有 test 区：{c}");
        assert!(c.contains("template IN ANY test"));
        assert!(c.contains("127.0.0.1"));
        assert!(
            c.contains("forward . 8.8.8.8 1.1.1.1"),
            "其余应转发公共 DNS"
        );
        // {{ .Name }} 是 CoreDNS 模板占位符，不能被 Rust 格式化吃掉
        assert!(c.contains("{{ .Name }}"), "模板占位符必须保留：{c}");
    }

    #[test]
    fn custom_upstreams_and_tld_dot_stripped() {
        let c = render_corefile(".dev", &["9.9.9.9"]);
        assert!(c.contains("dev {"));
        assert!(c.contains("forward . 9.9.9.9"));
        assert!(!c.contains(".dev {"), "前导点应被清理");
    }

    #[test]
    fn write_creates_file() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        write_corefile(&paths, "test", &[]).unwrap();
        let p = paths.etc().join("coredns").join("Corefile");
        assert!(p.is_file());
        let raw = std::fs::read_to_string(p).unwrap();
        assert!(raw.contains("template IN ANY test"));
    }
}
