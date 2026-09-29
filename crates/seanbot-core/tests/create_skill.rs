//! `create_skill` 工具的集成测试（走公开 API，不依赖模块内部实现）。

use std::fs;

use seanbot_core::{
    Tool, ToolContext, skill,
    tools::{CreateSkillTool, SkillTool},
};
use serde_json::json;

fn setup() -> (tempfile::TempDir, ToolContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolContext::new(dir.path().to_path_buf());
    ctx.data_dir = Some(dir.path().join("data"));
    (dir, ctx)
}

#[tokio::test]
async fn creates_a_project_skill_with_the_standard_skeleton() {
    let (dir, ctx) = setup();
    let out = CreateSkillTool
        .call(
            json!({"name": "release-check", "description": "发版前的检查清单"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        out.content.contains("已创建项目技能 release-check"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains("新会话"),
        "要提醒生效时机：{}",
        out.content
    );

    let path = dir
        .path()
        .join(skill::PROJECT_SUBDIR)
        .join("release-check")
        .join(skill::SKILL_FILE);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("---\nname: release-check\ndescription: 发版前的检查清单\n---"),
        "{text}"
    );
    for section in ["## 何时用", "## 步骤", "## 注意事项", "## 自检"] {
        assert!(text.contains(section), "骨架缺 {section}：{text}");
    }

    let listed = SkillTool.call(json!({}), &ctx).await.unwrap();
    assert!(
        listed.content.contains("release-check"),
        "{}",
        listed.content
    );

    let err = CreateSkillTool
        .call(
            json!({"name": "release-check", "description": "再来一次"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("已经存在"), "{err}");
    CreateSkillTool
        .call(
            json!({"name": "release-check", "description": "覆盖", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn rejects_bad_names_and_official_scope() {
    let (_dir, ctx) = setup();
    for bad in ["", "A", "有中文", "-lead", "trail-", "two--dash", "x"] {
        let err = CreateSkillTool
            .call(json!({"name": bad, "description": "d"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, seanbot_core::ToolError::InvalidArgs(_)),
            "{bad}: {err}"
        );
    }
    let err = CreateSkillTool
        .call(
            json!({"name": "ok-name", "scope": "official", "description": "d"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("官方"), "{err}");
    let err = CreateSkillTool
        .call(json!({"name": "ok-name", "description": "   "}), &ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("description"), "{err}");
}

#[tokio::test]
async fn global_scope_keeps_given_content() {
    let (dir, ctx) = setup();
    CreateSkillTool
        .call(
            json!({
                "name": "global-only",
                "scope": "global",
                "description": "只对自己生效",
                "content": "# 正文\n\n自己写的步骤"
            }),
            &ctx,
        )
        .await
        .unwrap();
    let path = dir
        .path()
        .join("data")
        .join(skill::GLOBAL_SUBDIR)
        .join("global-only")
        .join(skill::SKILL_FILE);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# 正文"), "{text}");
    assert!(text.contains("自己写的步骤"), "{text}");
    assert!(
        !text.contains("## 何时用"),
        "给了 content 就不该塞骨架：{text}"
    );
}

/// 官方技能随二进制分发、首次发现时释放，且标成「官方」。
#[tokio::test]
async fn official_skills_are_released_and_labelled() {
    let (_dir, ctx) = setup();
    let out = SkillTool.call(json!({}), &ctx).await.unwrap();
    assert!(out.content.contains("官方"), "{}", out.content);
    assert!(out.content.contains("write-skill"), "{}", out.content);
    assert!(out.content.contains("code-review"), "{}", out.content);

    let loaded = SkillTool
        .call(json!({"name": "write-skill"}), &ctx)
        .await
        .unwrap();
    assert!(loaded.content.contains("frontmatter"), "{}", loaded.content);
    assert!(
        loaded.content.contains("create_skill"),
        "{}",
        loaded.content
    );
}
