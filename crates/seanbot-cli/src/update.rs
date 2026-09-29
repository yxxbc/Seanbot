//! `sean update`：从 GitHub Release 下载新版本并替换自身。
//!
//! 资产命名与 `scripts/install.sh` 保持一致：`sean-<target>.tar.gz`（Windows 为 `.zip`）
//! 加一份 `SHA256SUMS`。
//!
//! 取版本与下载优先走 `gh`（认证、代理、限流都交给它），没装 gh 时回退到内置 HTTP；
//! `SEANBOT_DOWNLOAD_BASE` 指向本地目录时直接读文件，供离线测试。

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Context, bail};
use seanbot_core::config::{self, Config};
use sha2::{Digest, Sha256};

const REPO: &str = "yxxbc/Seanbot";
const API_LATEST: &str = "https://api.github.com/repos/yxxbc/Seanbot/releases/latest";
const DEFAULT_BASE: &str = "https://github.com/yxxbc/Seanbot/releases";
/// 下载超时；安装包只有几 MB，给足余量
const TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(check: bool, version: Option<String>) -> anyhow::Result<ExitCode> {
    let current = env!("CARGO_PKG_VERSION");
    let target = host_target()?;
    let version = match version {
        Some(v) => normalize_version(&v),
        None => latest_version().await?,
    };

    if !is_newer(&version, current) {
        println!("已是最新版本（v{current}）");
        return Ok(ExitCode::SUCCESS);
    }
    println!("发现新版本 v{version}（当前 v{current}，平台 {target}）");
    if check {
        println!("本次只检查，未下载。运行 `sean update` 即可更新。");
        return Ok(ExitCode::SUCCESS);
    }

    let exe = std::env::current_exe().context("找不到当前可执行文件的位置")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("可执行文件没有所在目录：{}", exe.display()))?
        .to_path_buf();
    // 暂存目录必须与目标同处一个文件系统，rename 才能原子替换
    let staging = dir.join(format!(".sean-update-{}", std::process::id()));
    fs::create_dir_all(&staging).with_context(|| {
        format!(
            "无法在 {} 下创建暂存目录（可能需要管理员权限）",
            dir.display()
        )
    })?;

    let result = install(&version, target, &staging, &exe).await;
    let _ = fs::remove_dir_all(&staging);
    result?;

    println!("✓ 已更新到 v{version}：{}", exe.display());
    Ok(ExitCode::SUCCESS)
}

async fn install(version: &str, target: &str, staging: &Path, exe: &Path) -> anyhow::Result<()> {
    let asset = format!("sean-{target}.tar.gz");
    let archive = staging.join(&asset);
    let sums = staging.join("SHA256SUMS");

    println!("下载 {asset} …");
    download(version, &asset, &archive).await?;
    download(version, "SHA256SUMS", &sums).await?;

    let expected = parse_sha256sums(&fs::read_to_string(&sums)?, &asset)?;
    let actual = sha256_file(&archive)?;
    if expected != actual {
        bail!("下载内容校验失败（期望 {expected}，实际 {actual}），已中止更新");
    }

    let unpacked = extract_binary(&archive, staging)?;
    replace(exe, &unpacked)?;
    Ok(())
}

/// 取回一个发布资产：本地覆盖目录（测试）→ gh → 内置 HTTP。
async fn download(version: &str, name: &str, dest: &Path) -> anyhow::Result<()> {
    if let Some(base) = local_base() {
        let source = base.join(format!("download/v{version}")).join(name);
        fs::copy(&source, dest)
            .with_context(|| format!("读取本地文件 {} 失败", source.display()))?;
        return Ok(());
    }
    if gh_available() {
        return gh_download(version, name, dest);
    }
    let url = format!("{}/download/v{version}/{name}", http_base());
    http_download(&url, dest).await
}

/// `SEANBOT_DOWNLOAD_BASE` 指向本地目录时返回它（离线测试用）。
fn local_base() -> Option<PathBuf> {
    std::env::var("SEANBOT_DOWNLOAD_BASE")
        .ok()
        .filter(|b| !b.starts_with("http://") && !b.starts_with("https://"))
        .map(PathBuf::from)
}

fn http_base() -> String {
    std::env::var("SEANBOT_DOWNLOAD_BASE").unwrap_or_else(|_| DEFAULT_BASE.to_string())
}

/// gh 是否可用。只探测一次。
fn gh_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("gh")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// 用 gh 下载发布资产：认证、代理与限流都由 gh 处理。
fn gh_download(version: &str, name: &str, dest: &Path) -> anyhow::Result<()> {
    let dir = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("下载目标没有所在目录：{}", dest.display()))?;
    let output = Command::new("gh")
        .args([
            "release",
            "download",
            &format!("v{version}"),
            "--repo",
            REPO,
            "--pattern",
            name,
            "--clobber",
        ])
        .arg("--dir")
        .arg(dir)
        .output()
        .context("执行 gh 失败")?;
    if !output.status.success() {
        bail!(
            "gh 下载 {name} 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // gh 按资产名落盘；调用方期望的路径可能不同（如暂存名），必要时改名
    let downloaded = dir.join(name);
    if downloaded != dest {
        fs::rename(&downloaded, dest)?;
    }
    Ok(())
}

/// 当前平台对应的发布资产三元组。
fn host_target() -> anyhow::Result<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        ("x86_64", "macos") => Ok("x86_64-apple-darwin"),
        ("aarch64", "linux") => Ok("aarch64-unknown-linux-gnu"),
        ("x86_64", "linux") => Ok("x86_64-unknown-linux-gnu"),
        (arch, os) => bail!(
            "暂不支持在 {arch}-{os} 上自更新（Windows 上无法替换正在运行的程序）；\
             请改用安装脚本：https://github.com/{REPO}#安装"
        ),
    }
}

async fn latest_version() -> anyhow::Result<String> {
    if gh_available()
        && let Ok(version) = gh_latest_version()
    {
        return Ok(version);
    }
    let response = client()
        .get(API_LATEST)
        .send()
        .await
        .context("查询最新版本失败（网络不可用？）")?;
    let status = response.status();
    if !status.is_success() {
        // GitHub 对匿名请求限流，403/429 基本可以确定是这个原因
        let hint = if status.as_u16() == 403 || status.as_u16() == 429 {
            "（GitHub API 匿名请求限流，稍后再试或用 `sean update --version <版本>`）"
        } else {
            ""
        };
        bail!("查询最新版本失败：HTTP {status}{hint}");
    }
    let body: serde_json::Value = response.json().await.context("解析版本信息失败")?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("版本信息里没有 tag_name"))?;
    Ok(normalize_version(tag))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("seanbot/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// 用 gh 查最新 tag（已登录时还能避开匿名限流）。
fn gh_latest_version() -> anyhow::Result<String> {
    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{REPO}/releases/latest"),
            "--jq",
            ".tag_name",
        ])
        .output()
        .context("执行 gh 失败")?;
    if !output.status.success() {
        bail!(
            "gh api 查询最新版本失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let tag = String::from_utf8(output.stdout).context("gh 输出不是有效文本")?;
    Ok(normalize_version(tag.trim()))
}

/// 下载一个地址到本地文件。
async fn http_download(url: &str, dest: &Path) -> anyhow::Result<()> {
    let response = client()
        .get(url)
        .send()
        .await
        .with_context(|| format!("下载 {url} 失败"))?;
    if !response.status().is_success() {
        bail!("下载 {url} 失败：HTTP {}", response.status());
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("读取 {url} 的响应体失败"))?;
    fs::write(dest, &bytes).with_context(|| format!("写入 {} 失败", dest.display()))?;
    Ok(())
}

fn normalize_version(v: &str) -> String {
    v.trim().trim_start_matches('v').to_string()
}

/// 主次修订三段比较；非数字段落按 0 处理。
fn parse_version(v: &str) -> (u64, u64, u64) {
    let normalized = normalize_version(v);
    let mut parts = normalized
        .split(['.', '-', '+'])
        .map(|p| p.parse().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn is_newer(remote: &str, current: &str) -> bool {
    parse_version(remote) > parse_version(current)
}

/// 从 `sha256sum` 格式的清单里取出某个文件的哈希（大小写不敏感）。
fn parse_sha256sums(text: &str, asset: &str) -> anyhow::Result<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name.trim_start_matches('*') == asset {
            return Ok(hash.to_lowercase());
        }
    }
    bail!("SHA256SUMS 里没有 {asset}")
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// 解出包里的 `sean` 并写到暂存目录。只认文件名，避免归档里的路径穿越。
fn extract_binary(archive: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let file = fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in tar.entries().context("读取安装包失败")? {
        let mut entry = entry.context("读取安装包失败")?;
        if entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "sean"))
            != Some(true)
        {
            continue;
        }
        let dest = dir.join("sean.new");
        entry
            .unpack(&dest)
            .with_context(|| format!("解压到 {} 失败", dest.display()))?;
        return Ok(dest);
    }
    bail!("安装包里没有 sean")
}

#[cfg(unix)]
fn replace(exe: &Path, new: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(new, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("设置 {} 权限失败", new.display()))?;
    fs::rename(new, exe).with_context(|| {
        format!(
            "替换 {} 失败（目录不可写？请用管理员权限重试，或改用安装脚本）",
            exe.display()
        )
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn replace(exe: &Path, _new: &Path) -> anyhow::Result<()> {
    bail!(
        "无法替换正在运行的程序：{}；请用安装脚本升级",
        exe.display()
    )
}

// ------------------------------------------------------------ 启动时的更新检查

/// 更新检查的缓存文件（放在数据目录里）。
const CHECK_FILE: &str = "update-check.json";
/// 缓存有效期：24 小时内不再联网查询。
const CHECK_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// 后台检查的网络超时：启动流程不该等它。
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// 设了这个环境变量就完全跳过启动检查。
const NO_CHECK_ENV: &str = "SEANBOT_NO_UPDATE_CHECK";

/// 检查是否开启：配置项与环境变量都允许关闭。
pub fn check_enabled(cfg: &Config) -> bool {
    cfg.ui.check_updates && std::env::var_os(NO_CHECK_ENV).is_none()
}

/// 启动路径用：只读缓存、不发网络请求；有新版本才返回提示。
pub fn cached_hint() -> Option<String> {
    let path = cache_path()?;
    let (_, latest) = read_cache_at(&path)?;
    hint_for(&latest)
}

/// 后台刷新缓存：24 小时内的缓存直接跳过；网络失败静默放弃（不影响任何功能）。
pub async fn refresh_cache() {
    let now = unix_now();
    if let Some(path) = cache_path()
        && let Some((checked_at, _)) = read_cache_at(&path)
        && cache_is_fresh(checked_at, now)
    {
        return;
    }
    let Ok(latest) = fetch_latest_quick().await else {
        return;
    };
    if let Some(path) = cache_path() {
        write_cache_at(&path, &latest, now);
    }
}

fn cache_is_fresh(checked_at: u64, now: u64) -> bool {
    now.saturating_sub(checked_at) < CHECK_TTL.as_secs()
}

fn cache_path() -> Option<PathBuf> {
    config::data_dir().ok().map(|dir| dir.join(CHECK_FILE))
}

fn read_cache_at(path: &Path) -> Option<(u64, String)> {
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((
        value.get("checked_at")?.as_u64()?,
        value.get("latest")?.as_str()?.to_string(),
    ))
}

fn write_cache_at(path: &Path, latest: &str, checked_at: u64) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let body = serde_json::json!({ "checked_at": checked_at, "latest": latest });
    let _ = fs::write(path, body.to_string());
}

/// 后台检查只走 HTTP（短超时），不走 `gh`：启动路径要快，失败也不打扰用户。
async fn fetch_latest_quick() -> anyhow::Result<String> {
    let response = reqwest::Client::builder()
        .user_agent(concat!("seanbot/", env!("CARGO_PKG_VERSION")))
        .timeout(CHECK_TIMEOUT)
        .build()
        .context("构建 HTTP 客户端失败")?
        .get(API_LATEST)
        .send()
        .await
        .context("查询最新版本失败")?;
    if !response.status().is_success() {
        bail!("查询最新版本失败：HTTP {}", response.status());
    }
    let body: serde_json::Value = response.json().await.context("解析版本信息失败")?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("版本信息里没有 tag_name"))?;
    Ok(normalize_version(tag))
}

/// 有新版本时的提示文案；已是最新（或版本号为空）返回 `None`。
fn hint_for(latest: &str) -> Option<String> {
    let current = env!("CARGO_PKG_VERSION");
    if latest.trim().is_empty() || !is_newer(latest, current) {
        return None;
    }
    Some(format!(
        "提示：发现新版本 v{latest}（当前 v{current}），运行 `sean update` 升级"
    ))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_normalized_and_compared() {
        assert_eq!(normalize_version("v0.2.0"), "0.2.0");
        assert_eq!(normalize_version(" 0.2.0 "), "0.2.0");
        assert!(is_newer("0.2.0", "0.1.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        // 预发布标记不参与比较，0.2.0-rc1 视同 0.2.0
        assert!(is_newer("0.2.0-rc1", "0.1.1"));
    }

    #[test]
    fn sha256sums_entries_are_matched() {
        let text = "\
aaaa1111  sean-aarch64-apple-darwin.tar.gz
BBBB2222 *sean-x86_64-unknown-linux-gnu.tar.gz
";
        assert_eq!(
            parse_sha256sums(text, "sean-aarch64-apple-darwin.tar.gz").unwrap(),
            "aaaa1111"
        );
        assert_eq!(
            parse_sha256sums(text, "sean-x86_64-unknown-linux-gnu.tar.gz").unwrap(),
            "bbbb2222"
        );
        assert!(parse_sha256sums(text, "sean-other.tar.gz").is_err());
        assert!(parse_sha256sums("", "sean.tar.gz").is_err());
    }

    #[test]
    fn sha256_matches_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn host_target_is_known_or_reported() {
        match host_target() {
            Ok(target) => assert!(target.contains('-'), "{target}"),
            Err(e) => assert!(e.to_string().contains("暂不支持")),
        }
    }

    #[test]
    fn hint_only_for_newer_versions() {
        assert!(hint_for("").is_none(), "查不到版本时不该提示");
        assert!(
            hint_for(env!("CARGO_PKG_VERSION")).is_none(),
            "同版本不该提示"
        );
        assert!(hint_for("0.0.1").is_none(), "更老的版本不该提示");
        let hint = hint_for("99.0.0").expect("更高的版本要提示");
        assert!(hint.contains("99.0.0"), "{hint}");
        assert!(hint.contains("sean update"), "{hint}");
    }

    #[test]
    fn cache_write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");

        assert!(read_cache_at(&path).is_none(), "没有缓存文件时读不到");
        write_cache_at(&path, "9.9.9", 1_700_000_000);
        assert_eq!(read_cache_at(&path), Some((1_700_000_000, "9.9.9".into())));

        // 缓存坏了也不影响任何功能：当作没查过
        fs::write(&path, "not json").unwrap();
        assert!(read_cache_at(&path).is_none());
    }

    #[test]
    fn cache_is_fresh_for_24_hours() {
        let now = 1_700_000_000u64;
        assert!(cache_is_fresh(now, now));
        assert!(cache_is_fresh(now - 23 * 3600, now));
        assert!(!cache_is_fresh(now - 25 * 3600, now));
        // 时钟回拨（缓存时间在未来）按刚查过处理，不反复联网
        assert!(cache_is_fresh(now + 3600, now));
    }
}
