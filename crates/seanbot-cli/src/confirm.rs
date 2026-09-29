//! 交互式权限确认：改动类工具在执行前问一句。

use std::{
    collections::HashSet,
    io::{self, Write},
    sync::Mutex,
};

use async_trait::async_trait;
use seanbot_core::{
    Decision, PermissionHandler, PermissionMode, PermissionRequest, Risk, SharedRuntime,
};

use crate::{format, render::SharedRenderer};

/// 提示里参数最多展示多少个字符。
const ARGS_CHARS: usize = 240;

/// 用户在确认提示里的回答。
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Once,
    Always,
    Deny(Option<String>),
}

/// 解析回答：回车或 `y` 允许一次，`a` 本会话总是允许，`n` 拒绝；
/// 其他任何输入都当作拒绝原因（用户常直接写"别删这个目录"）。
fn parse_answer(input: &str) -> Answer {
    let text = input.trim();
    match text.to_lowercase().as_str() {
        "" | "y" | "yes" => Answer::Once,
        "a" | "always" => Answer::Always,
        "n" | "no" => Answer::Deny(None),
        _ => Answer::Deny(Some(text.to_string())),
    }
}

/// 参数的展示文本；空参数返回 `None`。
fn describe_args(json: &str) -> Option<String> {
    if json.is_empty() || json == "null" || json == "{}" {
        return None;
    }
    Some(format::clip(&format::sanitize(json), ARGS_CHARS))
}

/// 在终端里询问是否允许执行某个工具。
///
/// 只读工具、YOLO 模式、以及本会话已选择「总是允许」的工具直接放行，不打扰用户。
pub struct Confirm<W: Write + Send> {
    runtime: SharedRuntime,
    renderer: SharedRenderer<W>,
    /// 本会话内已选择「总是允许」的工具名
    always: Mutex<HashSet<String>>,
}

impl<W: Write + Send> Confirm<W> {
    pub fn new(runtime: SharedRuntime, renderer: SharedRenderer<W>) -> Self {
        Self {
            runtime,
            renderer,
            always: Mutex::new(HashSet::new()),
        }
    }

    fn auto_allowed(&self, req: &PermissionRequest) -> bool {
        self.runtime.read().unwrap().permission_mode == PermissionMode::Yolo
            || req.risk == Risk::ReadOnly
            || self.always.lock().unwrap().contains(&req.tool)
    }

    /// 暂停动画、提问、收回终端。读 stdin 是阻塞的，用 `block_in_place` 让出 worker
    /// 线程（`#[tokio::main]` 默认多线程运行时）。
    fn ask_once(&self, req: &PermissionRequest) -> Answer {
        let _ = self.renderer.lock().unwrap().suspend();
        self.print_prompt(req);
        let answer = match read_line() {
            Some(line) => parse_answer(&line),
            // stdin 已结束（如被重定向）：不能默认放行
            None => Answer::Deny(Some("无法读取确认输入，未获授权".into())),
        };
        self.renderer.lock().unwrap().resume();
        answer
    }

    fn print_prompt(&self, req: &PermissionRequest) {
        let mut out = io::stdout();
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "⚠ 需要确认：{}",
            format::tool_label(&req.tool, &req.title)
        );
        if let Some(args) = describe_args(&req.args.to_string()) {
            let _ = writeln!(out, "  {args}");
        }
        let _ = write!(
            out,
            "允许执行？[回车/y] 允许一次 · [a] 本会话总是允许 · [n] 拒绝（也可直接写拒绝原因）› "
        );
        let _ = out.flush();
    }
}

#[async_trait]
impl<W: Write + Send + 'static> PermissionHandler for Confirm<W> {
    async fn ask(&self, req: PermissionRequest) -> Decision {
        if self.auto_allowed(&req) {
            return Decision::AllowOnce;
        }
        match tokio::task::block_in_place(|| self.ask_once(&req)) {
            Answer::Once => Decision::AllowOnce,
            Answer::Always => {
                self.always.lock().unwrap().insert(req.tool);
                Decision::AllowSession
            }
            Answer::Deny(reason) => Decision::Deny { reason },
        }
    }
}

fn read_line() -> Option<String> {
    let mut line = String::new();
    match io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seanbot_core::{RuntimeState, shared_runtime};
    use serde_json::json;

    #[test]
    fn answers_are_parsed() {
        assert_eq!(parse_answer(""), Answer::Once);
        assert_eq!(parse_answer(" y \n"), Answer::Once);
        assert_eq!(parse_answer("Y"), Answer::Once);
        assert_eq!(parse_answer("a"), Answer::Always);
        assert_eq!(parse_answer("n"), Answer::Deny(None));
        assert_eq!(
            parse_answer("别动那个目录"),
            Answer::Deny(Some("别动那个目录".into()))
        );
    }

    #[test]
    fn arguments_are_flattened_and_trimmed() {
        assert_eq!(describe_args("{}"), None);
        assert_eq!(describe_args("null"), None);
        assert_eq!(
            describe_args(&json!({"command": "cargo build"}).to_string()).unwrap(),
            r#"{"command":"cargo build"}"#
        );
        let long = describe_args(&json!({"command": "x".repeat(400)}).to_string()).unwrap();
        assert!(long.chars().count() <= ARGS_CHARS + 1, "{long}");
        assert!(long.ends_with('…'));
    }

    #[test]
    fn auto_allowed_covers_readonly_and_yolo() {
        let runtime = shared_runtime(RuntimeState::default());
        let renderer = crate::render::shared(crate::render::Renderer::new(
            Vec::new(),
            crate::render::RenderStyle {
                animate: false,
                color: false,
                show_reasoning: false,
                cols: None,
            },
            Box::new(std::time::Instant::now),
        ));
        let confirm = Confirm::new(runtime.clone(), renderer);
        let request = |risk| PermissionRequest {
            tool: "bash".into(),
            title: "cargo build".into(),
            risk,
            args: json!({"command": "cargo build"}),
        };
        assert!(confirm.auto_allowed(&request(Risk::ReadOnly)));
        assert!(!confirm.auto_allowed(&request(Risk::Mutating)));

        runtime.write().unwrap().permission_mode = PermissionMode::Yolo;
        assert!(confirm.auto_allowed(&request(Risk::Mutating)));

        runtime.write().unwrap().permission_mode = PermissionMode::Confirm;
        confirm.always.lock().unwrap().insert("bash".into());
        assert!(confirm.auto_allowed(&request(Risk::Mutating)));
        assert!(!confirm.auto_allowed(&PermissionRequest {
            tool: "edit".into(),
            ..request(Risk::Mutating)
        }));
    }
}
