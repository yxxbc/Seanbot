//! 交互式对话循环。

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Instant,
};

use rustyline::{DefaultEditor, error::ReadlineError};
use seanbot_core::{
    Agent, PromptEnv,
    config::{Config, history_path},
    session, system_prompt,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    format,
    journal::{self, Journal},
    render::{self, RenderStyle, Renderer},
    setup,
};

const HELP: &str = "\
/clear   清空当前对话
/new     开始新会话（换一个会话文件）
/resume  从列表中选择并恢复历史会话
/model   切换模型
/help    显示帮助
/exit    退出
行末输入 \\ 可换行继续输入；执行中按 Ctrl+C 中断当前任务；Ctrl+D 退出";

#[derive(Debug, PartialEq, Eq)]
enum Slash {
    Clear,
    New,
    Resume,
    Model,
    Help,
    Exit,
    Unknown(String),
}

fn parse_slash(input: &str) -> Slash {
    match input.split_whitespace().next().unwrap_or_default() {
        "/clear" => Slash::Clear,
        "/new" => Slash::New,
        "/resume" => Slash::Resume,
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

/// 运行一轮：渲染事件、处理 Ctrl+C，并把事件录进会话文件。返回本轮是否成功。
async fn run_turn(
    agent: &mut Agent,
    cfg: &Config,
    journal: &Arc<Mutex<Journal>>,
    input: String,
) -> bool {
    let (tx, mut rx) = mpsc::channel(256);
    let (ui_tx, ui_rx) = mpsc::channel(256);
    // 先建好会话文件：本轮工具里调用 perceive 时就能看到会话 ID
    journal.lock().unwrap().ensure_open();
    let renderer = Renderer::new(
        std::io::stdout(),
        RenderStyle::detect(cfg.ui.show_reasoning),
        Box::new(Instant::now),
    );
    let render_task = tokio::spawn(render::drive(renderer, ui_rx));
    // 落盘与渲染各消费一次：把内核事件复制一份转发给渲染器
    let tap_task = {
        let journal = journal.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Ok(mut journal) = journal.lock() {
                    journal.record(&event);
                }
                if ui_tx.send(event).await.is_err() {
                    break;
                }
            }
        })
    };
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
    let _ = tap_task.await;
    let _ = render_task.await;
    result.is_ok()
}

pub async fn run_once(
    agent: &mut Agent,
    cfg: &Config,
    journal: &Arc<Mutex<Journal>>,
    prompt: String,
) -> ExitCode {
    if run_turn(agent, cfg, journal, prompt).await {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub async fn run(
    agent: &mut Agent,
    cfg: &Config,
    journal: &Arc<Mutex<Journal>>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    println!(
        "Seanbot v{} · {}/{} · {}",
        env!("CARGO_PKG_VERSION"),
        cfg.provider,
        agent.model(),
        format::tilde_path(&cwd, dirs::home_dir().as_deref())
    );
    {
        let journal = journal.lock().unwrap();
        match (journal.is_recording(), journal.id()) {
            // 已恢复的会话由 main 报告过，这里不重复
            (true, Some(_)) => {}
            (true, None) => println!("本次对话会写入会话文件 · sean -c 可继续"),
            (false, _) => println!("本次对话不会保存（--no-session）"),
        }
    }
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
                    journal
                        .lock()
                        .unwrap()
                        .append_if_started(&session::Record::clear_now());
                    println!("已清空对话");
                }
                Slash::New => start_new(agent, journal, &cwd),
                Slash::Resume => {
                    if let Err(e) = resume_into(&mut editor, agent, journal, &cwd) {
                        println!("恢复会话失败：{e:#}");
                    }
                }
                Slash::Model => switch_model(agent, journal).await,
                Slash::Help => println!("{HELP}"),
                Slash::Exit => break,
                Slash::Unknown(cmd) => println!("未知命令：{cmd}（输入 /help 查看可用命令）"),
            }
            continue;
        }
        run_turn(agent, cfg, journal, input).await;
    }
    let _ = editor.save_history(&history);
    Ok(())
}

/// `/new`：换一段会话。系统提示词按当前环境重新生成，不沿用恢复来的旧提示词。
fn start_new(agent: &mut Agent, journal: &Arc<Mutex<Journal>>, cwd: &Path) {
    let system = system_prompt(&PromptEnv::detect(cwd));
    agent.restore(system.clone(), Vec::new());
    journal.lock().unwrap().start_new(agent.model(), system);
    println!("已开始新会话");
}

/// `/resume`：列出当前目录的会话并切换过去。返回是否真的切换了。
fn resume_into(
    editor: &mut DefaultEditor,
    agent: &mut Agent,
    journal: &Arc<Mutex<Journal>>,
    cwd: &Path,
) -> anyhow::Result<bool> {
    let Some(path) = pick_session(editor, journal, cwd)? else {
        return Ok(false);
    };
    let (loaded, writer) = session::resume_session(&path)?;
    let id = loaded.meta.id.clone();
    let messages = loaded.history.len();
    agent.restore(loaded.meta.system.clone(), loaded.history.clone());
    agent.set_model(loaded.model.clone());
    journal.lock().unwrap().bind(loaded.meta, writer);
    for warning in &loaded.warnings {
        println!("提示：{warning}");
    }
    println!("已恢复会话 {id}（{messages} 条消息）");
    Ok(true)
}

/// 用当前会话的行编辑器选一个已有会话。
///
/// 不能像启动时那样直接读 stdin：rustyline 已经把输入缓冲在自己手里，
/// 另开一条读路径会把后面几条命令一并吃掉。
fn pick_session(
    editor: &mut DefaultEditor,
    journal: &Arc<Mutex<Journal>>,
    cwd: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let sessions = journal.lock().unwrap().store().list(cwd)?;
    if sessions.is_empty() {
        println!("当前目录还没有会话记录");
        return Ok(None);
    }
    journal::print_sessions(&sessions);
    loop {
        let prompt = "输入序号（回车选最近一次）：";
        match editor.readline(prompt) {
            Ok(line) => match setup::parse_choice(&line, sessions.len(), Some(0)) {
                Some(i) => return Ok(Some(sessions[i].path.clone())),
                None => println!("请输入 1-{} 之间的序号（Ctrl+C 取消）", sessions.len()),
            },
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => {
                println!("已取消");
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
    }
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

async fn switch_model(agent: &mut Agent, journal: &Arc<Mutex<Journal>>) {
    let models = match agent.provider().list_models().await {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return println!("厂商未返回任何模型"),
        Err(e) => return println!("获取模型列表失败：{e}"),
    };
    let current = models.iter().position(|m| m.id == agent.model());
    match setup::choose_model(&models, current) {
        Ok(i) => {
            agent.set_model(models[i].id.clone());
            let model = agent.model().to_string();
            journal
                .lock()
                .unwrap()
                .append_if_started(&session::Record::Model {
                    model: model.clone(),
                });
            println!("已切换到 {model}");
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
        assert_eq!(parse_slash("/new"), Slash::New);
        assert_eq!(parse_slash("/resume"), Slash::Resume);
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
