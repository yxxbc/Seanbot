//! 交互式对话循环。

use std::{process::ExitCode, time::Instant};

use rustyline::{DefaultEditor, error::ReadlineError};
use seanbot_core::{
    Agent,
    config::{Config, history_path},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    format,
    render::{self, RenderStyle, Renderer},
    setup,
};

const HELP: &str = "\
/clear   清空对话
/model   切换模型
/help    显示帮助
/exit    退出
行末输入 \\ 可换行继续输入；执行中按 Ctrl+C 中断当前任务；Ctrl+D 退出";

#[derive(Debug, PartialEq, Eq)]
enum Slash {
    Clear,
    Model,
    Help,
    Exit,
    Unknown(String),
}

fn parse_slash(input: &str) -> Slash {
    match input.split_whitespace().next().unwrap_or_default() {
        "/clear" => Slash::Clear,
        "/model" => Slash::Model,
        "/help" => Slash::Help,
        "/exit" | "/quit" => Slash::Exit,
        other => Slash::Unknown(other.to_string()),
    }
}

/// 行末的 `\` 表示续行，返回去掉 `\` 后的内容。
fn continuation(line: &str) -> Option<&str> {
    line.strip_suffix('\\')
}

/// 运行一轮：渲染事件、处理 Ctrl+C。返回本轮是否成功。
async fn run_turn(agent: &mut Agent, cfg: &Config, input: String) -> bool {
    let (tx, rx) = mpsc::channel(256);
    let renderer = Renderer::new(
        std::io::stdout(),
        RenderStyle::detect(cfg.ui.show_reasoning),
        Box::new(Instant::now),
    );
    let render_task = tokio::spawn(render::drive(renderer, rx));
    let cancel = CancellationToken::new();
    let watcher = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        })
    };
    let result = agent.run_turn(input, tx, cancel).await;
    watcher.abort();
    let _ = render_task.await;
    result.is_ok()
}

pub async fn run_once(agent: &mut Agent, cfg: &Config, prompt: String) -> ExitCode {
    if run_turn(agent, cfg, prompt).await {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub async fn run(agent: &mut Agent, cfg: &Config) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    println!(
        "Seanbot v{} · {}/{} · {}",
        env!("CARGO_PKG_VERSION"),
        cfg.provider,
        agent.model(),
        format::tilde_path(&cwd, dirs::home_dir().as_deref())
    );
    println!("输入 /help 查看命令");

    let mut editor = DefaultEditor::new()?;
    let history = history_path()?;
    if let Some(dir) = history.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = editor.load_history(&history);

    let mut interrupted = false;
    loop {
        let input = match read_input(&mut editor) {
            Ok(Some(line)) => {
                interrupted = false;
                line
            }
            Ok(None) => break,
            Err(ReadlineError::Interrupted) => {
                if interrupted {
                    break;
                }
                interrupted = true;
                println!("（再按一次 Ctrl+C 退出）");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(trimmed);
        if trimmed.starts_with('/') {
            match parse_slash(trimmed) {
                Slash::Clear => {
                    agent.clear();
                    println!("已清空对话");
                }
                Slash::Model => switch_model(agent).await,
                Slash::Help => println!("{HELP}"),
                Slash::Exit => break,
                Slash::Unknown(cmd) => println!("未知命令：{cmd}（输入 /help 查看可用命令）"),
            }
            continue;
        }
        run_turn(agent, cfg, input).await;
    }
    let _ = editor.save_history(&history);
    Ok(())
}

/// 读取一条输入，支持行末 `\` 续行。`Ok(None)` 表示 Ctrl+D。
fn read_input(editor: &mut DefaultEditor) -> Result<Option<String>, ReadlineError> {
    let mut buf = String::new();
    let mut prompt = "› ";
    loop {
        match editor.readline(prompt) {
            Ok(line) => match continuation(&line) {
                Some(head) => {
                    buf.push_str(head);
                    buf.push('\n');
                    prompt = "… ";
                }
                None => {
                    buf.push_str(&line);
                    return Ok(Some(buf));
                }
            },
            Err(ReadlineError::Eof) => return Ok(if buf.is_empty() { None } else { Some(buf) }),
            Err(e) => return Err(e),
        }
    }
}

async fn switch_model(agent: &mut Agent) {
    let models = match agent.provider().list_models().await {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return println!("厂商未返回任何模型"),
        Err(e) => return println!("获取模型列表失败：{e}"),
    };
    let current = models.iter().position(|m| m.id == agent.model());
    match setup::choose_model(&models, current) {
        Ok(i) => {
            agent.set_model(models[i].id.clone());
            println!("已切换到 {}", agent.model());
        }
        Err(e) => println!("未切换模型：{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands() {
        assert_eq!(parse_slash("/clear"), Slash::Clear);
        assert_eq!(parse_slash("/model  "), Slash::Model);
        assert_eq!(parse_slash("/quit"), Slash::Exit);
        assert_eq!(parse_slash("/foo bar"), Slash::Unknown("/foo".into()));
    }

    #[test]
    fn line_continuation() {
        assert_eq!(continuation("第一行\\"), Some("第一行"));
        assert_eq!(continuation("普通一行"), None);
    }
}
