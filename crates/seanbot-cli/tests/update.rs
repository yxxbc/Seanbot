//! `sean update` 的端到端测试：用本地目录伪造一个 Release，不联网。
//!
//! `SEANBOT_DOWNLOAD_BASE` 指向本地目录时 `update` 直接按路径取文件，
//! 因此这里可以完整走一遍「查版本 → 下载 → 校验 → 解包 → 替换自身」。

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};

const VERSION: &str = "9.9.9";
/// 解包出来后应该被写成可执行文件的内容（不必是真的二进制，比对字节即可）
const PAYLOAD: &[u8] = b"#!/bin/sh\necho fake sean\n";

fn host_target() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 伪造 `<root>/download/v<version>/{sean-<target>.tar.gz,SHA256SUMS}`。
fn fake_release(root: &Path, target: &str, payload: &[u8]) -> PathBuf {
    let dir = root.join(format!("download/v{VERSION}"));
    fs::create_dir_all(&dir).unwrap();
    let asset = format!("sean-{target}.tar.gz");

    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut header = tar::Header::new_gnu();
    header.set_size(payload.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append_data(&mut header, "sean", payload).unwrap();
    let gz = builder.into_inner().unwrap();
    let bytes = gz.finish().unwrap();
    fs::write(dir.join(&asset), &bytes).unwrap();
    fs::write(
        dir.join("SHA256SUMS"),
        format!("{}  {asset}\n", sha256(&bytes)),
    )
    .unwrap();
    root.to_path_buf()
}

/// 把待测二进制复制到临时目录，替换的是副本，不动 cargo 的构建产物。
fn copy_binary(dir: &Path) -> PathBuf {
    let dest = dir.join("sean");
    fs::copy(env!("CARGO_BIN_EXE_sean"), &dest).unwrap();
    dest
}

fn run_update(binary: &Path, base: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary)
        .args(args)
        .env("SEANBOT_DOWNLOAD_BASE", base)
        .output()
        .unwrap()
}

#[test]
fn update_replaces_the_running_binary() {
    let Some(target) = host_target() else {
        return; // 不支持的平台（Windows）由 host_target 自行报错
    };
    let dir = tempfile::tempdir().unwrap();
    let release = fake_release(dir.path(), target, PAYLOAD);
    let binary = copy_binary(dir.path());
    let before = fs::read(&binary).unwrap();
    assert_ne!(before, PAYLOAD);

    let out = run_update(&binary, &release, &["update", "--version", VERSION]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("✓ 已更新到 v9.9.9"), "{stdout}");

    assert_eq!(fs::read(&binary).unwrap(), PAYLOAD, "二进制应被替换");
    // 暂存目录要清理干净
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".sean-update-"))
        .collect();
    assert!(leftovers.is_empty(), "残留：{leftovers:?}");
}

#[test]
fn check_only_reports_without_touching_anything() {
    let Some(target) = host_target() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let release = fake_release(dir.path(), target, PAYLOAD);
    let binary = copy_binary(dir.path());
    let before = fs::read(&binary).unwrap();

    let out = run_update(
        &binary,
        &release,
        &["update", "--check", "--version", VERSION],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("发现新版本 v9.9.9"), "{stdout}");
    assert!(stdout.contains("只检查"), "{stdout}");
    assert_eq!(fs::read(&binary).unwrap(), before, "--check 不应改动文件");
}

#[test]
fn older_or_equal_version_is_left_alone() {
    let Some(target) = host_target() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let release = fake_release(dir.path(), target, PAYLOAD);
    let binary = copy_binary(dir.path());
    let before = fs::read(&binary).unwrap();

    // 当前版本是 0.1.1，比它更小的版本号不会触发更新
    let out = run_update(&binary, &release, &["update", "--version", "0.0.1"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("已是最新版本"), "{stdout}");
    assert_eq!(fs::read(&binary).unwrap(), before);
}

#[test]
fn corrupted_archive_is_rejected() {
    let Some(target) = host_target() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let release = fake_release(dir.path(), target, PAYLOAD);
    // 篡改安装包，让校验和不匹配
    let asset = release
        .join(format!("download/v{VERSION}"))
        .join(format!("sean-{target}.tar.gz"));
    fs::write(&asset, b"not the archive you signed").unwrap();
    let binary = copy_binary(dir.path());
    let before = fs::read(&binary).unwrap();

    let out = run_update(&binary, &release, &["update", "--version", VERSION]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "校验失败时不应成功");
    assert!(stderr.contains("校验失败"), "{stderr}");
    assert_eq!(
        fs::read(&binary).unwrap(),
        before,
        "校验失败时不应替换二进制"
    );
}
