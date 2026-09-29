//! `sean skills`：查看会注入系统提示词的指令文件与可用技能。

use std::process::ExitCode;

use anyhow::Result;
use seanbot_core::{config, instruction, skill};

use crate::format::human_bytes;

pub fn run() -> Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let data_dir = config::data_dir().ok();

    println!("工作目录：{}", cwd.display());

    let instructions = instruction::discover(&cwd, data_dir.as_deref());
    println!(
        "\n指令文件（会话开始时读一次并注入系统提示词，共 {} 个）",
        instructions.len()
    );
    if instructions.is_empty() {
        println!("  （无）");
        println!(
            "  放置位置：工作目录或任意上层目录的 {}；全局用 <数据目录>/{}",
            instruction::FILE_NAMES.join(" / "),
            instruction::FILE_NAMES[0]
        );
    }
    for file in &instructions {
        println!(
            "  [{}] {}  {}{}",
            file.scope.label(),
            file.path.display(),
            human_bytes(file.content.len() as u64),
            if file.truncated {
                "（已截断）"
            } else {
                ""
            }
        );
    }

    let discovery = skill::discover(&cwd, data_dir.as_deref());
    println!(
        "\n技能（提示词里只列名字与说明，正文由 skill 工具按需加载，共 {} 个）",
        discovery.skills.len()
    );
    if discovery.skills.is_empty() {
        println!("  （无）");
        println!(
            "  放置位置：<工作目录>/{}/<名>/{}（项目，就近优先）或 <数据目录>/{}/<名>/{}（全局）",
            skill::PROJECT_SUBDIR,
            skill::SKILL_FILE,
            skill::GLOBAL_SUBDIR,
            skill::SKILL_FILE
        );
    }
    for item in &discovery.skills {
        println!(
            "  [{}] {}  {}  {}",
            item.scope.label(),
            item.name,
            human_bytes(item.bytes),
            item.description
        );
        println!("        {}", item.path.display());
    }
    for warning in &discovery.warnings {
        println!("  提示：{warning}");
    }

    Ok(ExitCode::SUCCESS)
}
