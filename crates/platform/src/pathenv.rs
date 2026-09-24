//! 运行时 bin 目录注入系统 PATH。
//!
//! - Windows：改写 `HKCU\Environment` 的 `Path` 值（用户级，无需管理员），
//!   写完广播 `WM_SETTINGCHANGE`，让新开的终端立刻生效（否则要重登才看到）。
//! - macOS：往 `~/.zshrc`（以及已存在的 `~/.bash_profile`）写托管标记块，
//!   与 hosts 的标记块同一套路，可整块回滚。
//!
//! 这里只负责「读原文 / 写原文」，绝不解析语义之外的东西：PATH 是极敏感的
//! 系统设置，任何解析/合并/去重都放在 core 的纯函数里做，便于单测。

use crate::Result;

/// 托管标记块（macOS shell profile 用），与 hosts 保持一致风格
pub const PATH_BEGIN: &str = "# BEGIN NiceEnv (managed PATH)";
pub const PATH_END: &str = "# END NiceEnv (managed PATH)";
/// 旧版产品名（NiceServBay）写入的标记：解析/合并时同样识别，避免残留旧块
pub const PATH_BEGIN_LEGACY: &str = "# BEGIN NiceServBay (managed PATH)";
pub const PATH_END_LEGACY: &str = "# END NiceServBay (managed PATH)";

/// Windows 用户环境变量所在的注册表位置（写用户级，不需要管理员）
#[cfg(windows)]
pub const USER_ENV_SUBKEY: &str = "Environment";

/// PATH 的当前值 + 注册表类型。
/// `reg_type` 必须原样写回：`REG_EXPAND_SZ`（含 `%SystemRoot%` 这类引用）
/// 被降级成 `REG_SZ` 会让整段 PATH 里的变量引用永久失效。
#[derive(Debug, Clone)]
pub struct RawPath {
    pub value: String,
    /// Windows: REG_SZ(1) / REG_EXPAND_SZ(2)；macOS 恒为 0
    pub reg_type: u32,
}

/// 读取用户 PATH 原文（不做展开、不做分割）。
pub fn read_user_path() -> Result<RawPath> {
    #[cfg(windows)]
    {
        windows_path::read()
    }
    #[cfg(not(windows))]
    {
        // macOS 以 shell profile 为准，没有「注册表原文」概念
        Ok(RawPath { value: String::new(), reg_type: 0 })
    }
}

/// 写回用户 PATH。`reg_type` 传 `read_user_path` 返回的原值以保持类型。
/// 会顺带广播环境变量变更。
pub fn write_user_path(value: &str, reg_type: u32) -> Result<()> {
    #[cfg(windows)]
    {
        windows_path::write(value, reg_type)?;
        windows_path::broadcast_change();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (value, reg_type);
        Err(crate::PlatformError::Unsupported(
            "macOS 请改用托管块接口写 shell profile".into(),
        ))
    }
}

/// Windows 专用的 PATH 可达性检测：是否有生效中的用户 Path。
/// macOS 恒返回 false（由 profile 块判断）。
pub fn has_user_path() -> bool {
    #[cfg(windows)]
    {
        windows_path::read().map(|p| !p.value.trim().is_empty()).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/* ================= macOS：shell profile 托管块 ================= */

/// 需要维护的 shell 启动文件（按优先级）。zsh 是 macOS 10.15+ 默认 shell；
/// bash 用户若已有 ~/.bash_profile 也一并维护，避免「切了 shell 就没了」。
#[cfg(not(windows))]
pub fn shell_profiles() -> Vec<std::path::PathBuf> {
    let home = match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(h) => h,
        None => return Vec::new(),
    };
    let mut out = vec![home.join(".zshrc")];
    let bash_profile = home.join(".bash_profile");
    if bash_profile.exists() {
        out.push(bash_profile);
    }
    out
}

/// 读取某个 profile 全文（不存在返回空串）
#[cfg(not(windows))]
pub fn read_profile(path: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(crate::PlatformError::Io(format!(
            "读取 {} 失败：{e}",
            path.display()
        ))),
    }
}

/// 把托管块写入某 profile（保留块外内容，块已存在则整体替换）。
/// 纯逻辑在 `merge_profile_content`，这里只负责落盘。
#[cfg(not(windows))]
pub fn write_profile_managed_block(path: &std::path::Path, dirs: &[String]) -> Result<()> {
    let original = read_profile(path)?;
    let merged = merge_profile_content(&original, dirs);
    if merged == original {
        return Ok(()); // 幂等：内容一致就不碰文件（也不刷新 mtime）
    }
    std::fs::write(path, merged)
        .map_err(|e| crate::PlatformError::Io(format!("写入 {} 失败：{e}", path.display())))
}

/// 纯函数：把托管块合并进 profile 文本。
/// 移除已有的托管块（含重复块），然后按需追加新块；`dirs` 为空则只做移除。
///
/// 跨平台编译：这是纯字符串处理，挂在 `not(windows)` 下会导致 Windows 上
/// 无法单测，而 macOS 的 profile 逻辑恰恰是最需要测的（本机 CI 在 Windows）。
pub fn merge_profile_content(original: &str, dirs: &[String]) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in original.lines() {
        let t = line.trim();
        if t == PATH_BEGIN || t == PATH_BEGIN_LEGACY {
            in_block = true;
            continue;
        }
        if t == PATH_END || t == PATH_END_LEGACY {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    if dirs.is_empty() {
        return out;
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&render_profile_block(dirs));
    out
}

fn render_profile_block(dirs: &[String]) -> String {
    let mut s = String::from(PATH_BEGIN);
    s.push('\n');
    // 逐条前置：靠后的条目最终排在更前面，所以倒序输出，
    // 保证 dirs[0] 在 PATH 中排最前（与 Windows 侧的「前置」语义一致）
    for d in dirs.iter().rev() {
        s.push_str(&format!("export PATH=\"{d}:$PATH\"\n"));
    }
    s.push_str(PATH_END);
    s.push('\n');
    s
}

/// 从 profile 文本里解析出托管块声明的目录，按优先级返回（首元素在 PATH 中最靠前）。
///
/// 需要在文件顺序基础上反转：`render_profile_block` 为了让 dirs[0] 排最前，
/// 是倒序写入的，读回来必须反转才能还原同一个优先级序列。
/// 同样是纯函数，跨平台可测。
pub fn parse_profile_managed_dirs(content: &str) -> Vec<String> {
    let mut in_block = false;
    let mut file_order = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t == PATH_BEGIN || t == PATH_BEGIN_LEGACY {
            in_block = true;
            continue;
        }
        if t == PATH_END || t == PATH_END_LEGACY {
            in_block = false;
            continue;
        }
        if in_block {
            if let Some(rest) = t.strip_prefix("export PATH=\"") {
                if let Some(dir) = rest.split(":$PATH").next() {
                    file_order.push(dir.to_string());
                }
            }
        }
    }
    file_order.reverse();
    file_order
}

/// 当前 profile 里托管块声明的目录（用于状态展示与漂移检测）
#[cfg(not(windows))]
pub fn read_profile_managed_dirs(path: &std::path::Path) -> Vec<String> {
    match read_profile(path) {
        Ok(c) => parse_profile_managed_dirs(&c),
        Err(_) => Vec::new(),
    }
}

/* ================= Windows 注册表实现 ================= */

#[cfg(windows)]
mod windows_path {
    use super::RawPath;
    use crate::PlatformError;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_READ, KEY_SET_VALUE, REG_EXPAND_SZ,
    };

    const SUBKEY: &str = super::USER_ENV_SUBKEY;
    const VALUE: &str = "Path";

    // RegGetValueW 的 flags（这些常量在部分 windows-sys 版本里没导出，用字面量并注明）
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const RRF_RT_REG_EXPAND_SZ: u32 = 0x0000_0004;
    /// 关键：不要展开 `%VAR%`，必须拿到原始字符串才能原样写回
    const RRF_NOEXPAND: u32 = 0x1000_0000;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 读原始字节缓冲转 UTF-16 字符串（去掉结尾 NUL）
    fn wide_to_string(buf: &[u8]) -> String {
        let units: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect();
        String::from_utf16_lossy(&units)
    }

    pub fn read() -> Result<RawPath, PlatformError> {
        unsafe {
            let subkey = wide(SUBKEY);
            let value = wide(VALUE);
            let mut buf = vec![0u8; 64 * 1024];
            let mut size: u32 = buf.len() as u32;
            let mut reg_type: u32 = 0;
            let rc = RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                &mut reg_type,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut size,
            );
            if rc != ERROR_SUCCESS {
                // 值不存在（用户从未设过用户级 PATH）按空处理，不算错误
                if rc == 2 /* ERROR_FILE_NOT_FOUND */ {
                    return Ok(RawPath { value: String::new(), reg_type: REG_EXPAND_SZ });
                }
                return Err(PlatformError::Win(format!(
                    "读取用户 PATH 失败（注册表错误 {rc}）"
                )));
            }
            Ok(RawPath {
                value: wide_to_string(&buf[..size as usize]),
                reg_type: if reg_type == 0 { REG_EXPAND_SZ } else { reg_type },
            })
        }
    }

    pub fn write(value: &str, reg_type: u32) -> Result<(), PlatformError> {
        unsafe {
            let subkey = wide(SUBKEY);
            let mut hkey: HKEY = std::ptr::null_mut();
            let rc = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_READ | KEY_SET_VALUE,
                &mut hkey,
            );
            if rc != ERROR_SUCCESS {
                return Err(PlatformError::Win(format!(
                    "打开 HKCU\\Environment 失败（错误 {rc}）"
                )));
            }
            let name = wide(VALUE);
            let ws = wide(value);
            let ty = if reg_type == 0 { REG_EXPAND_SZ } else { reg_type };
            let rc = RegSetValueExW(
                hkey,
                name.as_ptr(),
                0,
                ty,
                ws.as_ptr() as *const u8,
                (ws.len() * 2) as u32,
            );
            RegCloseKey(hkey);
            if rc != ERROR_SUCCESS {
                return Err(PlatformError::Win(format!(
                    "写入用户 PATH 失败（注册表错误 {rc}）"
                )));
            }
            Ok(())
        }
    }

    /// 广播 WM_SETTINGCHANGE，让资源管理器与新开的终端重新读取环境变量。
    /// 不做这一步的话，用户必须注销/重登才能看到 PATH 变化。
    pub fn broadcast_change() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let param: Vec<u16> = "Environment"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut result: usize = 0;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                2000,
                &mut result,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn merge_appends_block_and_keeps_existing_content() {
        let original = "export LANG=en_US.UTF-8\n";
        let out = merge_profile_content(original, &s(&["/a/bin", "/b/bin"]));
        assert!(out.contains("export LANG=en_US.UTF-8"));
        assert!(out.contains(PATH_BEGIN));
        assert!(out.contains(PATH_END));
        // dirs[0] 必须排最前：倒序输出，最后一行 export 是 dirs[0]
        let a = out.find("/a/bin").unwrap();
        let b = out.find("/b/bin").unwrap();
        assert!(a > b, "dirs[0] 应写在更靠后（从而在 PATH 中更靠前）");
    }

    #[test]
    fn merge_is_idempotent() {
        let dirs = s(&["/a/bin", "/b/bin"]);
        let once = merge_profile_content("export FOO=1\n", &dirs);
        let twice = merge_profile_content(&once, &dirs);
        assert_eq!(once, twice, "重复应用不应产生第二个托管块");
        assert_eq!(once.matches(PATH_BEGIN).count(), 1);
    }

    #[test]
    fn merge_empty_dirs_removes_block_only() {
        let dirs = s(&["/a/bin"]);
        let with_block = merge_profile_content("export FOO=1\n", &dirs);
        let removed = merge_profile_content(&with_block, &[]);
        assert!(!removed.contains(PATH_BEGIN));
        assert!(!removed.contains("/a/bin"));
        assert!(removed.contains("export FOO=1"));
    }

    #[test]
    fn merge_dedupes_duplicate_blocks() {
        let dirs = s(&["/a/bin"]);
        let block = render_profile_block(&dirs);
        let doubled = format!("export FOO=1\n{block}{block}");
        let out = merge_profile_content(&doubled, &dirs);
        assert_eq!(out.matches(PATH_BEGIN).count(), 1, "重复块应被收敛为一个");
    }

    #[test]
    fn parse_roundtrips_merged_dirs() {
        let dirs = s(&["/a/bin", "/b/bin"]);
        let out = merge_profile_content("", &dirs);
        assert_eq!(parse_profile_managed_dirs(&out), dirs);
    }

    #[test]
    fn parse_ignores_content_outside_block() {
        let text = "export PATH=\"/evil/bin:$PATH\"\n";
        assert!(parse_profile_managed_dirs(text).is_empty());
    }

    #[test]
    fn remove_leaves_no_trailing_garbage_when_block_was_only_content() {
        let dirs = s(&["/a/bin"]);
        let only_block = merge_profile_content("", &dirs);
        let cleaned = merge_profile_content(&only_block, &[]);
        assert!(cleaned.trim().is_empty(), "只剩空行，实际：{cleaned:?}");
    }
}
