//! `skill` 工具：列出可用技能，或按需加载某个技能的正文。
//!
//! 技能在会话开始时被列进系统提示词（只有 name + description），正文在这里按需读取——
//! 既让模型知道有什么能力可用，又不把一堆手册塞满上下文。

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};

use crate::{
    skill,
    tool::{Risk, Tool, ToolContext, ToolError, ToolOutput, opt_str},
};

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".into(),
            description: "查看或加载技能。不带 name：列出所有可用技能（来源、名称、说明、路径）。带 name：返回该技能 SKILL.md 的全文，并列出技能目录下的其它文件（可以用 read 打开）。技能是用户或团队写好的操作手册：做对应任务前先加载它，按里面的步骤做，不要凭印象自己发挥。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "技能名；省略则列出全部技能"}
                }
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn title(&self, args: &Value) -> String {
        match args.get("name").and_then(Value::as_str) {
            Some(name) if !name.trim().is_empty() => format!("skill {name}"),
            _ => "skill 列表".to_string(),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let discovery = skill::discover(&ctx.cwd, ctx.data_dir.as_deref());
        let name = opt_str(&args, "name")?.unwrap_or("").trim().to_string();
        if name.is_empty() {
            return Ok(list(&discovery));
        }

        let Some(found) = skill::find(&discovery.skills, &name) else {
            return Err(ToolError::Failed(unknown(&name, &discovery)));
        };
        let loaded = skill::load(found).map_err(|e| {
            ToolError::Failed(format!("读取技能 {} 失败：{e}", found.path.display()))
        })?;

        let mut lines = vec![
            format!("# 技能 {}", found.name),
            format!(
                "来源：{} · 路径：{}",
                found.scope.label(),
                found.path.display()
            ),
            String::new(),
            loaded.content.trim_end().to_string(),
        ];
        if loaded.truncated {
            lines.push(String::new());
            lines.push(format!(
                "（正文过长已截断，用 read 打开 {} 看全文）",
                found.path.display()
            ));
        }
        if !loaded.siblings.is_empty() {
            lines.push(String::new());
            lines.push("技能目录下的其它文件（用 read 打开绝对路径）：".to_string());
            for sibling in &loaded.siblings {
                lines.push(format!("- {}", found.dir.join(sibling).display()));
            }
        }
        let mut output = ToolOutput::new(lines.join("\n"), format!("加载技能 {}", found.name));
        output.preview = output.content.lines().take(3).map(String::from).collect();
        Ok(output)
    }
}

fn list(discovery: &skill::Discovery) -> ToolOutput {
    let mut lines = Vec::new();
    if discovery.skills.is_empty() {
        lines.push("当前没有发现任何技能。".to_string());
        lines.push(String::new());
        lines.push("技能放在这两处（目录形式 <名>/SKILL.md，或单文件 <名>.md，frontmatter 里写 name 与 description）：".to_string());
        lines.push("- 项目：<工作目录>/.seanbot/skills/".to_string());
        lines.push("- 全局：<数据目录>/skills/".to_string());
    } else {
        lines.push(format!("可用技能 {} 个：", discovery.skills.len()));
        for item in &discovery.skills {
            lines.push(format!(
                "- {}（{}）：{}\n  路径：{}",
                item.name,
                item.scope.label(),
                item.description,
                item.path.display()
            ));
        }
        lines.push(String::new());
        lines.push("用 skill + name 加载正文，再按里面的步骤做。".to_string());
    }
    for warning in &discovery.warnings {
        lines.push(format!("提示：{warning}"));
    }
    let summary = format!("{} 个技能", discovery.skills.len());
    let mut output = ToolOutput::new(lines.join("\n"), summary);
    output.preview = output.content.lines().take(3).map(String::from).collect();
    output
}

fn unknown(name: &str, discovery: &skill::Discovery) -> String {
    if discovery.skills.is_empty() {
        return format!(
            "没有名为 {name} 的技能，当前也没发现任何技能；技能放在 <工作目录>/.seanbot/skills/<名>/SKILL.md 或 <数据目录>/skills/<名>/SKILL.md"
        );
    }
    let names: Vec<&str> = discovery.skills.iter().map(|s| s.name.as_str()).collect();
    format!("没有名为 {name} 的技能。可用：{}", names.join("、"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.data_dir = Some(dir.path().join("data"));
        (dir, ctx)
    }

    fn add_skill(cwd: &std::path::Path, name: &str, front: &str, body: &str) {
        let dir = cwd.join(skill::PROJECT_SUBDIR).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(skill::SKILL_FILE),
            format!("---\n{front}\n---\n{body}"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn lists_when_no_name_given() {
        let (dir, ctx) = setup();
        let out = SkillTool.call(json!({}), &ctx).await.unwrap();
        assert!(
            out.content.contains("当前没有发现任何技能"),
            "{}",
            out.content
        );
        assert!(out.content.contains(".seanbot/skills"), "{}", out.content);

        add_skill(
            dir.path(),
            "fix-imports",
            "name: fix-imports\ndescription: 修导入顺序",
            "# 步骤\n1. 看\n",
        );
        let out = SkillTool.call(json!({}), &ctx).await.unwrap();
        assert!(out.content.contains("fix-imports"), "{}", out.content);
        assert!(out.content.contains("修导入顺序"), "{}", out.content);
        assert!(out.content.contains("项目"), "{}", out.content);
    }

    #[tokio::test]
    async fn loads_body_and_resource_files() {
        let (dir, ctx) = setup();
        add_skill(
            dir.path(),
            "deploy",
            "name: deploy\ndescription: 发布",
            "# 发布步骤\n先跑测试\n",
        );
        let skill_dir = dir.path().join(skill::PROJECT_SUBDIR).join("deploy");
        fs::write(skill_dir.join("checklist.md"), "- [ ] 版本号\n").unwrap();

        let out = SkillTool
            .call(json!({"name": "deploy"}), &ctx)
            .await
            .unwrap();
        assert!(out.content.contains("# 技能 deploy"), "{}", out.content);
        assert!(out.content.contains("先跑测试"), "{}", out.content);
        assert!(out.content.contains("checklist.md"), "{}", out.content);
        assert_eq!(out.summary, "加载技能 deploy");

        let err = SkillTool
            .call(json!({"name": "nope"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("可用：deploy"), "{err}");
    }

    #[tokio::test]
    async fn reads_skills_from_global_dir_too() {
        let (dir, ctx) = setup();
        let global = dir
            .path()
            .join("data")
            .join(skill::GLOBAL_SUBDIR)
            .join("audit");
        fs::create_dir_all(&global).unwrap();
        fs::write(
            global.join(skill::SKILL_FILE),
            "---\nname: audit\ndescription: 审计\n---\n正文\n",
        )
        .unwrap();

        let out = SkillTool.call(json!({}), &ctx).await.unwrap();
        assert!(out.content.contains("audit"), "{}", out.content);
        assert!(out.content.contains("全局"), "{}", out.content);
    }

    #[test]
    fn is_read_only_and_titles_are_short() {
        assert!(SkillTool.read_only_call(&json!({})));
        assert_eq!(SkillTool.title(&json!({})), "skill 列表");
        assert_eq!(SkillTool.title(&json!({"name": "deploy"})), "skill deploy");
        assert_eq!(SkillTool.title(&json!({"name": "  "})), "skill 列表");
    }
}
