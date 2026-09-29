//! `sean kb`：查看知识库与更新内置知识库。

use std::{path::Path, process::ExitCode};

use anyhow::{Result, bail};
use seanbot_core::{
    config::Config,
    kb::{self, Scope},
};

/// `sean kb [list|update]`
pub async fn run(action: &str, cfg: &Config) -> Result<ExitCode> {
    let builtin = kb::builtin_dir()?;
    let custom = kb::custom_dir()?;
    match action.trim() {
        "" | "list" | "ls" => list(&builtin, &custom)?,
        "update" => update(&builtin, cfg).await?,
        other => bail!("未知动作：{other}（可用 list、update）"),
    }
    Ok(ExitCode::SUCCESS)
}

/// 列出两个目录与全部条目；顺带释放首次使用时该有的内嵌条目。
fn list(builtin: &Path, custom: &Path) -> Result<()> {
    let released = kb::ensure_builtin(builtin)?;
    if !released.is_empty() {
        println!(
            "已释放 {} 个内置条目到 {}",
            released.len(),
            builtin.display()
        );
    }

    println!("知识库");
    println!(
        "  内置：{}（官方维护，只读；sean kb update 更新）",
        builtin.display()
    );
    println!("  外置：{}（你或 agent 自建，可写）", custom.display());

    for scope in [Scope::Builtin, Scope::Custom] {
        let entries = kb::list(builtin, custom, scope)?;
        println!("\n{}（{} 条）", scope.label(), entries.len());
        if entries.is_empty() {
            println!(
                "  {}",
                match scope {
                    Scope::Custom => "（空）让 sean 用 kb_add 新建，或自己往这个目录里放 .md",
                    _ => "（空）执行 sean kb update 从官方地址拉取",
                }
            );
            continue;
        }
        for entry in entries {
            println!(
                "  {:<44} {:>7}  {}",
                entry.name,
                human_bytes(entry.bytes),
                entry.title
            );
        }
    }
    Ok(())
}

/// 从远程拉取内置知识库的最新内容：官方是什么，本地就同步成什么。
async fn update(builtin: &Path, cfg: &Config) -> Result<()> {
    let url = kb::base_url(cfg);
    println!("从 {url} 更新内置知识库…");
    let report = kb::update(builtin, &url).await?;
    for (name, change) in &report.changed {
        println!("  {} {}", change.label(), name);
    }
    for name in &report.removed {
        println!("  移除 {name}");
    }
    println!("{}", report.summary());
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
