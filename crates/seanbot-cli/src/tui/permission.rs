//! TUI 的权限处理器（设计书 §4.8）：把确认请求送进界面，等用户选择。
//!
//! 规则：
//! - YOLO 模式、只读工具：直接放行（黑名单在更前面，仍然生效）
//! - 本会话已记住的规则：放行（bash 按命令前两个词；edit 按"工作目录内的文件修改"）
//! - 其余：交给界面弹确认框；本轮被取消时回传的通道被丢弃，按**拒绝**处理

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use seanbot_core::{
    Decision, PermissionHandler, PermissionMode, PermissionRequest, Risk, SharedRuntime, is_within,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// 一次待确认的请求 + 回传决定的通道。
#[derive(Debug)]
pub struct Ask {
    pub request: PermissionRequest,
    pub reply: oneshot::Sender<Decision>,
    /// 是否提供"本会话不再询问"（改动工作目录之外的 edit 不提供）
    pub rememberable: bool,
    /// 界面显示用的几行预览
    pub preview: Vec<String>,
}

/// 本会话记住的授权规则。
#[derive(Debug, Default, Clone)]
pub struct Rules {
    /// 整个工具放行（例如"工作目录内的文件修改"）
    allowed_tools: HashSet<String>,
    /// bash 的命令前缀（前两个词），按词比对，避免 `cargo testx` 混进来
    bash_prefixes: Vec<Vec<String>>,
}

impl Rules {
    /// 这条请求是否已被记住的规则放行。
    pub fn allows(&self, request: &PermissionRequest) -> bool {
        if self.allowed_tools.contains(&request.tool) {
            return true;
        }
        if request.tool == "bash" {
            let command = request.args.get("command").and_then(Value::as_str);
            return match command {
                Some(command) => self
                    .bash_prefixes
                    .iter()
                    .any(|prefix| prefix_matches(prefix, command)),
                None => false,
            };
        }
        false
    }

    /// 记住这条请求（用户在确认框里选了"本会话不再询问"）。
    pub fn remember(&mut self, request: &PermissionRequest) {
        if request.tool == "bash"
            && let Some(command) = request.args.get("command").and_then(Value::as_str)
        {
            let words: Vec<String> = command
                .split_whitespace()
                .take(2)
                .map(str::to_string)
                .collect();
            if !words.is_empty() && !self.bash_prefixes.contains(&words) {
                self.bash_prefixes.push(words);
            }
            return;
        }
        self.allowed_tools.insert(request.tool.clone());
    }
}

/// 命令前缀匹配：按词比对，边界对齐。
pub fn prefix_matches(prefix: &[String], command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.len() < prefix.len() {
        return false;
    }
    prefix
        .iter()
        .zip(words.iter())
        .all(|(wanted, actual)| wanted == actual)
}

/// TUI 的权限处理器。
pub struct TuiPermission {
    runtime: SharedRuntime,
    tx: mpsc::UnboundedSender<Ask>,
    rules: Arc<Mutex<Rules>>,
    cwd: PathBuf,
}

impl TuiPermission {
    pub fn new(
        runtime: SharedRuntime,
        tx: mpsc::UnboundedSender<Ask>,
        rules: Arc<Mutex<Rules>>,
        cwd: PathBuf,
    ) -> Self {
        Self {
            runtime,
            tx,
            rules,
            cwd,
        }
    }
}

#[async_trait]
impl PermissionHandler for TuiPermission {
    async fn ask(&self, request: PermissionRequest) -> Decision {
        let yolo = self
            .runtime
            .read()
            .map(|state| state.permission_mode == PermissionMode::Yolo)
            .unwrap_or(false);
        if yolo || request.risk == Risk::ReadOnly {
            return Decision::AllowOnce;
        }
        if self
            .rules
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .allows(&request)
        {
            return Decision::AllowOnce;
        }

        let rememberable = targets_inside(&self.cwd, &request);
        let preview = preview_lines(&request);
        let (reply, rx) = oneshot::channel();
        let ask = Ask {
            request,
            reply,
            rememberable,
            preview,
        };
        if self.tx.send(ask).is_err() {
            // 界面不在了（正在退出）：按拒绝处理
            return Decision::Deny { reason: None };
        }
        // 本轮被取消时界面会丢掉这条请求，发送端随之关闭 → 视为拒绝
        rx.await.unwrap_or(Decision::Deny { reason: None })
    }
}

/// 改动工作目录之外的 edit 必须每次确认，且不给"不再询问"。
fn targets_inside(cwd: &Path, request: &PermissionRequest) -> bool {
    if request.tool != "edit" {
        return true;
    }
    match request.args.get("path").and_then(Value::as_str) {
        Some(raw) => is_within(cwd, Path::new(raw)),
        None => true,
    }
}

/// 确认框里显示的几行预览。
fn preview_lines(request: &PermissionRequest) -> Vec<String> {
    const MAX_LINES: usize = 20;
    let mut lines = vec![request.title.clone()];
    match request.tool.as_str() {
        "bash" => {
            if let Some(command) = request.args.get("command").and_then(Value::as_str) {
                lines.clear();
                lines.push(format!("$ {command}"));
            }
        }
        "edit" => {
            if let Some(path) = request.args.get("path").and_then(Value::as_str) {
                lines.push(format!("文件：{path}"));
            }
            if let Some(old) = request.args.get("old_string").and_then(Value::as_str) {
                lines.push(format!("- {}", clip(old, 80)));
            }
            if let Some(new) = request.args.get("new_string").and_then(Value::as_str) {
                lines.push(format!("+ {}", clip(new, 80)));
            }
        }
        _ => {}
    }
    lines.truncate(MAX_LINES);
    lines
}

fn clip(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or_default();
    if first.chars().count() <= max {
        first.to_string()
    } else {
        let head: String = first.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seanbot_core::{RuntimeState, shared_runtime};
    use serde_json::json;

    fn runtime(yolo: bool) -> SharedRuntime {
        let mut state = RuntimeState::default();
        if yolo {
            state.permission_mode = PermissionMode::Yolo;
        }
        shared_runtime(state)
    }

    fn request(tool: &str, args: Value, risk: Risk) -> PermissionRequest {
        PermissionRequest {
            tool: tool.to_string(),
            title: format!("{tool} 想要执行"),
            risk,
            args,
        }
    }

    fn new_handler(
        yolo: bool,
        rules: Arc<Mutex<Rules>>,
    ) -> (TuiPermission, mpsc::UnboundedReceiver<Ask>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            TuiPermission::new(runtime(yolo), tx, rules, PathBuf::from("/tmp/proj")),
            rx,
        )
    }

    #[test]
    fn remembered_bash_prefix_matches_by_word() {
        let mut rules = Rules::default();
        rules.remember(&request(
            "bash",
            json!({"command": "cargo test --workspace"}),
            Risk::Mutating,
        ));
        assert!(rules.allows(&request(
            "bash",
            json!({"command": "cargo test -p seanbot-core"}),
            Risk::Mutating
        )));
        assert!(
            !rules.allows(&request(
                "bash",
                json!({"command": "cargo testx"}),
                Risk::Mutating
            )),
            "单词边界要对齐"
        );
        assert!(!rules.allows(&request(
            "bash",
            json!({"command": "rm -rf /"}),
            Risk::Mutating
        )));
    }

    #[test]
    fn remembered_edit_covers_the_tool_but_not_bash() {
        let mut rules = Rules::default();
        rules.remember(&request(
            "edit",
            json!({"path": "/tmp/proj/a.rs"}),
            Risk::Mutating,
        ));
        assert!(rules.allows(&request("edit", json!({}), Risk::Mutating)));
        assert!(!rules.allows(&request("bash", json!({"command": "ls"}), Risk::Mutating)));
    }

    #[tokio::test]
    async fn readonly_and_yolo_never_ask() {
        let rules = Arc::new(Mutex::new(Rules::default()));
        let (handler, mut rx) = new_handler(false, rules.clone());
        assert_eq!(
            handler
                .ask(request("read", json!({}), Risk::ReadOnly))
                .await,
            Decision::AllowOnce
        );

        let (yolo, _rx) = new_handler(true, rules);
        assert_eq!(
            yolo.ask(request("bash", json!({"command": "rm x"}), Risk::Mutating))
                .await,
            Decision::AllowOnce
        );
        assert!(rx.try_recv().is_err(), "自动放行不该打扰界面");
    }

    #[tokio::test]
    async fn decisions_travel_back_from_the_ui() {
        let rules = Arc::new(Mutex::new(Rules::default()));
        let (handler, mut rx) = new_handler(false, rules);
        let asking = tokio::spawn(async move {
            handler
                .ask(request(
                    "bash",
                    json!({"command": "cargo test"}),
                    Risk::Mutating,
                ))
                .await
        });
        let ask = rx.recv().await.expect("应当收到确认请求");
        assert!(ask.rememberable, "bash 一律可以记住");
        assert!(ask.preview[0].contains("cargo test"), "{:?}", ask.preview);
        ask.reply.send(Decision::AllowSession).unwrap();
        assert_eq!(asking.await.unwrap(), Decision::AllowSession);
    }

    #[tokio::test]
    async fn cancelled_turns_deny() {
        let rules = Arc::new(Mutex::new(Rules::default()));
        let (handler, mut rx) = new_handler(false, rules);
        let asking = tokio::spawn(async move {
            handler
                .ask(request(
                    "edit",
                    json!({"path": "/etc/hosts"}),
                    Risk::Mutating,
                ))
                .await
        });
        let ask = rx.recv().await.expect("应当收到确认请求");
        assert!(!ask.rememberable, "工作目录之外的 edit 不给不再询问");
        drop(ask); // 模拟本轮被取消
        assert_eq!(asking.await.unwrap(), Decision::Deny { reason: None });
    }
}
