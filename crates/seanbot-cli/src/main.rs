mod confirm;
mod format;
mod journal;
mod kb;
mod markdown;
mod render;
mod repl;
mod setup;
mod skills;
mod tui;
mod update;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use seanbot_core::{
    PermissionMode,
    config::Config,
    session::{self, LoadedSession, SessionError, SessionStore},
};

use journal::Journal;
use render::{RenderStyle, Renderer};

#[derive(Parser)]
#[command(name = "sean", version, about = "Seanbot —— 终端里的 AI 助手")]
struct Cli {
    /// 单次提问：打印回答后退出
    #[arg(short = 'p', long = "print", value_name = "问题")]
    prompt: Option<String>,

    /// 继续当前目录最近一次会话
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    continue_session: bool,

    /// 恢复会话：给出会话 ID；省略 ID 则从列表中选
    #[arg(short = 'r', long = "resume", value_name = "会话ID", num_args = 0..=1)]
    resume: Option<Option<String>>,

    /// 不把本次对话写入会话文件
    #[arg(long = "no-session", conflicts_with = "session")]
    no_session: bool,

    /// 把本次对话写入会话文件（`-p` 单次提问默认不写）
    #[arg(long = "session")]
    session: bool,

    /// 跳过工具确认，直接执行（黑名单仍然生效）
    #[arg(long = "yolo")]
    yolo: bool,

    /// 临时覆盖本次使用的模型
    #[arg(long, global = true, value_name = "模型")]
    model: Option<String>,

    /// 记录模型请求、响应与用量到 JSONL 文件（包含完整对话内容）
    #[arg(long, value_name = "文件")]
    trace: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 配置厂商、API key 与模型
    Config,
    /// 列出当前厂商的模型
    Models,
    /// 查看会注入系统提示词的指令文件与可用技能
    Skills,
    /// 知识库：列出条目，或更新内置知识库
    Kb {
        /// 动作：list（默认）列出条目，update 从官方地址更新内置知识库
        #[arg(value_name = "动作", default_value = "list")]
        action: String,
    },
    /// 检查并更新到最新版本
    Update {
        /// 只检查是否有新版本，不下载
        #[arg(long)]
        check: bool,
        /// 更新到指定版本（默认最新）
        #[arg(long, value_name = "版本")]
        version: Option<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("错误：{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Some(Command::Config) => {
            setup::wizard(setup::load_config()?.unwrap_or_default()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Models) => {
            let cfg = setup::ensure_config().await?;
            setup::print_models(&cfg).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Skills) => skills::run(),
        Some(Command::Kb { action }) => {
            let cfg = setup::load_config()?.unwrap_or_default();
            kb::run(&action, &cfg).await
        }
        Some(Command::Update { check, version }) => update::run(check, version).await,
        None => converse(cli).await,
    }
}

/// 交互模式与 `-p` 单次模式共用的入口：构造 Agent、恢复会话、决定是否落盘。
async fn converse(cli: Cli) -> anyhow::Result<ExitCode> {
    let cfg = setup::ensure_config().await?;
    let cwd = std::env::current_dir()?;
    // 交互模式默认落盘；`-p` 单次提问默认不写（常用于脚本），要留档就用 --session
    let recording = !cli.no_session
        && (cli.prompt.is_none() || cli.session || cli.continue_session || cli.resume.is_some());

    let store = SessionStore::open_default()?;
    let target = resolve_target(&store, &cwd, &cli)?;

    // 交互模式才做确认提示；`-p` 走非交互策略（只读放行，改动类需 --yolo）
    let renderer = cli.prompt.is_none().then(|| {
        render::shared(Renderer::new(
            std::io::stdout(),
            RenderStyle::detect(cfg.ui.show_reasoning),
            Box::new(Instant::now),
        ))
    });
    let mut agent = setup::build_agent(
        &cfg,
        cli.model.as_deref(),
        cli.trace.as_deref(),
        renderer.clone(),
    )?;
    if cli.yolo {
        agent.runtime().write().unwrap().permission_mode = PermissionMode::Yolo;
    }
    if let Some(path) = &cli.trace {
        eprintln!("模型 trace 已启用：{}", path.display());
    }

    let resumed = match &target {
        Some(path) => {
            let (loaded, writer) = session::resume_session(path)
                .with_context(|| format!("恢复会话 {} 失败", path.display()))?;
            report_env_changes(&loaded, &cfg, &cwd);
            agent.restore(loaded.meta.system.clone(), loaded.history.clone());
            agent.set_model(loaded.model.clone());
            Some((loaded, writer))
        }
        None => None,
    };

    let mut journal = Journal::new(
        store,
        agent.runtime(),
        cwd.clone(),
        cfg.provider.clone(),
        agent.model(),
        agent.system_prompt(),
        recording,
    );
    if let Some((loaded, writer)) = resumed {
        // 用 stderr：`sean -r ID -p "问题"` 的 stdout 只应留下回答
        eprintln!(
            "已恢复会话 {}（{} 条消息）",
            loaded.meta.id,
            loaded.history.len()
        );
        for warning in &loaded.warnings {
            eprintln!("提示：{warning}");
        }
        journal.bind(loaded.meta, writer);
    }

    let journal = Arc::new(Mutex::new(journal));
    match cli.prompt {
        Some(prompt) => Ok(repl::run_once(&mut agent, &cfg, &journal, prompt).await),
        None => {
            // 启动时顺带查一次新版本：后台跑、失败静默，结果在下一个提示符前打印
            let (hint_tx, hint_rx) = tokio::sync::mpsc::unbounded_channel();
            if update::check_enabled(&cfg) {
                tokio::spawn(async move {
                    update::refresh_cache().await;
                    if let Some(hint) = update::cached_hint() {
                        let _ = hint_tx.send(hint);
                    }
                });
            }
            // 迁移期逃生门：TUI 还没到功能对等，设 SEANBOT_REPL=1 回到逐行 REPL
            if std::env::var_os("SEANBOT_REPL").is_some() {
                repl::run(
                    &mut agent,
                    &cfg,
                    &journal,
                    renderer.expect("交互模式一定有渲染器"),
                    hint_rx,
                )
                .await?;
            } else if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                tui::run(&mut agent, &cfg, &journal, hint_rx).await?;
            } else {
                anyhow::bail!(
                    "当前输出不是终端，无法启动交互界面；请用 `sean -p \"问题\"` 单轮提问"
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// 要恢复哪个会话文件：`-c` 取当前目录最近一次，`-r ID` 按 ID 找，`-r` 不带值则从列表里选。
fn resolve_target(store: &SessionStore, cwd: &Path, cli: &Cli) -> anyhow::Result<Option<PathBuf>> {
    if cli.continue_session {
        return match store.latest(cwd)? {
            Some(summary) => Ok(Some(summary.path)),
            None => {
                println!("当前目录还没有会话记录，开始新会话");
                Ok(None)
            }
        };
    }
    match &cli.resume {
        None => Ok(None),
        Some(None) => Ok(journal::pick_session(store, cwd)?),
        Some(Some(id)) => {
            let id = id.trim();
            match store.find(cwd, id) {
                Ok(path) => Ok(Some(path)),
                Err(SessionError::NotFound(_)) => {
                    anyhow::bail!("找不到会话 {id}；用 `sean -r` 可以列出当前目录的会话")
                }
                Err(e) => Err(e.into()),
            }
        }
    }
}

/// 会话记录的环境与当前不一致时给出提示（都不阻断恢复）。
fn report_env_changes(loaded: &LoadedSession, cfg: &Config, cwd: &Path) {
    if loaded.meta.provider != cfg.provider {
        eprintln!(
            "提示：该会话记录于厂商 {}，当前配置是 {}，本次按当前配置请求",
            loaded.meta.provider, cfg.provider
        );
    }
    if !journal::same_dir(&loaded.meta.cwd, cwd) {
        eprintln!(
            "提示：该会话创建于 {}，当前工作目录是 {}，工具将在当前目录下执行",
            loaded.meta.cwd.display(),
            cwd.display()
        );
    }
}
