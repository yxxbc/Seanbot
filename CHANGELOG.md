# 更新日志

Seanbot 的所有重要变更都会记录在本文件中。

- 格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)
- 版本号遵循 [语义化版本（SemVer）](https://semver.org/lang/zh-CN/)
- 维护约定：
  - `feat` → 「新增」，`fix` → 「修复」，`perf` → 「性能」，`refactor` → 「重构」
  - 破坏性变更单独标注「⚠️ 破坏性变更」
  - 用户可见的变更应在同一个 PR 中更新本文件

## [未发布]

### 新增

- 项目初始化：Rust 单 crate 骨架（edition 2024，命令名 `sean`）
- 仓库基建：`.gitignore`、更新日志、Issue 与 PR 模板、提交信息规范检查（git hooks）
- 内置官方知识库（`kb/`）
  - `kb/AboutSeanbot/01-Seanbot.md`：Seanbot 自我介绍（能力、用法、作者信息）
  - `kb/SeanbotTools/01-KernelTool.md`：内核基础工具（`bash`、`read`、`edit`、`search`）参数与行为说明
- 品牌资源：`pics/Seanbot-icon.svg`、`pics/Seanbot-app-icon.svg`
