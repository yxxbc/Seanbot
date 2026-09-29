//! `create_skill` 工具：按官方标准脚手架一个新技能（只写外置：项目或全局）。
//!
//! 标准本身写在官方技能 `write-skill` 里（随二进制分发），这个工具负责把骨架落盘：
//! frontmatter 两项 + 固定的小节骨架，让人和 agent 都按同一套结构写。

use std::fs;

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};

use crate::{
    skill,
    tool::{Risk, Tool, ToolContext, ToolError, ToolOutput, opt_bool, opt_str, str_arg},
};

/// 名字规则：小写 kebab-case，2-64 字符，只用 a-z 0-9 -。
const MAX_NAME_CHARS: usize = 64;
/// description 的长度上限。
const MAX_DESCRIPTION_CHARS: usize = 200;

pub struct CreateSkillTool;

#[async_trait]
impl Tool for CreateSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "create_skill".into(),
            description: "按官方标准创建一个新技能（生成 SKILL.md 骨架）。只写外置技能：scope=project 落在 <工作目录>/.seanbot/skills/，scope=global 落在 <数据目录>/skills/；官方技能只读，不能创建。生成后请把正文补实（触发条件、可执行的步骤、容易踩的坑、自检方式）——标准的完整说明在官方技能 write-skill 里，可用 skill 工具加载。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "技能名（小写 kebab-case，如 release-check）"},
                    "description": {"type": "string", "description": "一句话说明什么时候用它；会进系统提示词的技能清单"},
                    "scope": {"type": "string", "enum": ["project", "global"], "description": "project 写进当前项目（默认），global 写进数据目录只对自己生效"},
                    "content": {"type": "string", "description": "技能正文（markdown）；省略则生成标准骨架"},
                    "overwrite": {"type": "boolean", "description": "已存在同名技能时是否覆盖，默认 false"}
                },
                "required": ["name", "description"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    fn title(&self, args: &Value) -> String {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        match args.get("scope").and_then(Value::as_str) {
            Some(scope) => format!("create_skill {name} ({scope})"),
            None => name.to_string(),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = valid_name(str_arg(&args, "name")?)?;
        let description = str_arg(&args, "description")?.trim().to_string();
        if description.is_empty() {
            return Err(ToolError::InvalidArgs("description 不能为空".into()));
        }
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "description 太长了（上限 {MAX_DESCRIPTION_CHARS} 字符）：写一句话说明什么时候用它就行"
            )));
        }
        let scope = match opt_str(&args, "scope")?.unwrap_or("project").trim() {
            "project" => skill::Scope::Project,
            "global" => skill::Scope::Global,
            other => {
                return Err(ToolError::InvalidArgs(format!(
                    "scope 只能是 project 或 global，收到「{other}」；官方技能随 Seanbot 分发、只读，不能创建"
                )));
            }
        };
        let overwrite = opt_bool(&args, "overwrite")?.unwrap_or(false);
        let content = opt_str(&args, "content")?.unwrap_or("").trim().to_string();

        let root = match scope {
            skill::Scope::Project => ctx.cwd.join(skill::PROJECT_SUBDIR),
            _ => ctx
                .data_dir
                .clone()
                .ok_or_else(|| ToolError::Failed("取不到数据目录，无法创建全局技能".into()))?
                .join(skill::GLOBAL_SUBDIR),
        };
        let dir = root.join(&name);
        let path = dir.join(skill::SKILL_FILE);
        if path.is_file() && !overwrite {
            return Err(ToolError::Failed(format!(
                "{} 已经存在；要改内容请用 edit，确实要重写骨架再传 overwrite=true",
                path.display()
            )));
        }

        let used_skeleton = content.is_empty();
        let body = if used_skeleton {
            skeleton(&name, &description)
        } else {
            content
        };
        let text = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
        fs::create_dir_all(&dir)
            .map_err(|e| ToolError::Failed(format!("创建 {} 失败：{e}", dir.display())))?;
        fs::write(&path, &text)
            .map_err(|e| ToolError::Failed(format!("写入 {} 失败：{e}", path.display())))?;

        // 别的来源已有同名技能时提醒一句（同名时 项目 > 全局 > 官方）
        let clash = skill::discover(&ctx.cwd, ctx.data_dir.as_deref())
            .skills
            .into_iter()
            .any(|item| item.name == name && item.path != path);

        let mut message = format!(
            "已创建{}技能 {name}（{} 行）\n路径：{}",
            scope.label(),
            text.lines().count(),
            path.display()
        );
        if clash {
            message
                .push_str("\n注意：已有同名技能，同名时按 项目 > 全局 > 官方 取优先级最高的那个");
        }
        if used_skeleton {
            message
                .push_str("\n已生成标准骨架，请把「何时用 / 步骤 / 注意事项 / 自检」补成实际内容");
        }
        message.push_str(
            "\n提示：新技能要开新会话才会出现在「可用技能」清单里；当前会话可以用 read 直接读它。",
        );
        Ok(ToolOutput {
            content: message,
            summary: format!("create_skill {name}"),
            preview: Vec::new(),
            is_error: false,
        })
    }
}

/// 校验并返回技能名。
fn valid_name(raw: &str) -> Result<String, ToolError> {
    let name = raw.trim();
    let ok = (2..=MAX_NAME_CHARS).contains(&name.chars().count())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !ok {
        return Err(ToolError::InvalidArgs(format!(
            "技能名不合法：{raw}。要求 2-64 个字符，只用小写字母、数字与 -，不以 - 开头结尾、不出现连续 --，例如 release-check"
        )));
    }
    Ok(name.to_string())
}

/// 标准骨架：小节固定，内容留给创建者补。
fn skeleton(name: &str, description: &str) -> String {
    format!(
        "# {name}\n\n{description}\n\n## 何时用\n\n- （什么情况下该加载这个技能）\n\n## 步骤\n\n1. （可执行的动作：具体命令、文件、判断）\n2. \n\n## 注意事项\n\n- （这一步容易错在哪、报错长什么样、怎么绕）\n\n## 自检\n\n- （做完怎么确认真的成了：跑什么、看到什么）"
    )
}
