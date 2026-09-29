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
    Decision, PermissionHandler, PermissionMode, PermissionRequest, Risk, SharedRuntime, UserEvent,
    is_within,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// 一次待确认的请求 + 回传决定的通道。
///
/// 回传通道由**运行时**（主循环）持有：界面只发 `UserEvent::PermissionDecision`，
/// 拿不到通道、也就不可能绕过运行时自己答。`Ask::split` 就是这条分界线。
#[derive(Debug)]
pub struct Ask {
    pub request: PermissionRequest,
    pub reply: oneshot::Sender<Decision>,
    /// 是否提供"本会话不再询问"（改动工作目录之外的 edit 不提供）
    pub rememberable: bool,
    /// 界面显示用的几行预览
    pub preview: Vec<String>,
}

/// 界面要显示的一次授权请求（不含回传通道）。
#[derive(Debug, Clone)]
pub struct AskView {
    /// 回指用：答复时把工具名发回来，运行时才知道给哪次询问
    pub tool: String,
    pub request: PermissionRequest,
    pub rememberable: bool,
    pub preview: Vec<String>,
}

impl Ask {
    /// 拆成"运行时持有的回传通道"和"界面要显示的部分"。
    pub fn split(self) -> (oneshot::Sender<Decision>, AskView) {
        let Ask {
            request,
            reply,
            rememberable,
            preview,
        } = self;
        let view = AskView {
            tool: request.tool.clone(),
            request,
            rememberable,
            preview,
        };
        (reply, view)
    }
}

/// 运行时的"待答复"名单：内核在等哪几次授权。
///
/// 界面发来 `UserEvent::PermissionDecision` 后，由这里按**工具名**配对回传——
/// 确认框还没点、内核已经换了工具时就不会答错（同一个工具按先后顺序配对）。
/// 名单被丢掉（本轮结束/被取消/进程退出）时里面的通道随之关闭，内核按**拒绝**处理。
#[derive(Default)]
pub struct PendingAsks {
    waiting: Vec<(String, oneshot::Sender<Decision>)>,
}

impl PendingAsks {
    /// 记下一次正在等待的授权。
    pub fn push(&mut self, tool: impl Into<String>, reply: oneshot::Sender<Decision>) {
        self.waiting.push((tool.into(), reply));
    }

    /// 把界面的答复交给最早的那次同名询问。
    ///
    /// 返回 false 表示没有对应的询问（例如授权已经超时、或用户答的是上一轮的），
    /// 调用方可以忽略——界面说什么都不该让内核状态跑偏。
    pub fn resolve(&mut self, event: &UserEvent) -> bool {
        let UserEvent::PermissionDecision {
            tool,
            allow,
            reason,
        } = event
        else {
            return false;
        };
        let Some(index) = self.waiting.iter().position(|(name, _)| name == tool) else {
            return false;
        };
        let (_, reply) = self.waiting.remove(index);
        let decision = if *allow {
            Decision::AllowOnce
        } else {
            Decision::Deny {
                reason: reason.clone(),
            }
        };
        reply.send(decision).is_ok()
    }

    /// 还有几次询问没答复（运行时本身不需要，留给测试断言配对结果）。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }
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
        // 运行时拿到回传通道，界面只拿显示用的部分
        let (reply, view) = rx.recv().await.expect("应当收到确认请求").split();
        assert!(view.rememberable, "bash 一律可以记住");
        assert!(view.preview[0].contains("cargo test"), "{:?}", view.preview);
        assert_eq!(view.tool, "bash", "答复要靠工具名回指");
        reply.send(Decision::AllowSession).unwrap();
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
        let (reply, view) = ask.split();
        assert!(!view.rememberable, "工作目录之外的 edit 不给不再询问");
        drop(reply); // 模拟本轮被取消 / 待答复名单被丢弃
        assert_eq!(asking.await.unwrap(), Decision::Deny { reason: None });
    }

    fn decision(tool: &str, allow: bool, reason: Option<&str>) -> UserEvent {
        UserEvent::PermissionDecision {
            tool: tool.into(),
            allow,
            reason: reason.map(str::to_string),
        }
    }

    /// 答复按**工具名**配对：同名的按先后顺序，没有对应询问的直接忽略。
    #[tokio::test]
    async fn answers_are_matched_to_the_asking_tool() {
        let mut pending = PendingAsks::default();
        let (bash_tx, bash_rx) = oneshot::channel();
        let (edit_tx, edit_rx) = oneshot::channel();
        pending.push("bash", bash_tx);
        pending.push("edit", edit_tx);
        assert_eq!(pending.len(), 2);

        // 答的是 edit：只有 edit 那次询问被答复
        assert!(pending.resolve(&decision("edit", false, Some("别动生产"))));
        assert_eq!(
            edit_rx.await.unwrap(),
            Decision::Deny {
                reason: Some("别动生产".into())
            }
        );
        assert_eq!(pending.len(), 1, "答复过的不该再配对一次");

        // 答的是没在等的工具：忽略，不报错
        assert!(!pending.resolve(&decision("write", true, None)));
        assert!(
            !pending.resolve(&UserEvent::Cancel),
            "取消不是授权答复，不该被当成答复"
        );

        assert!(pending.resolve(&decision("bash", true, None)));
        assert_eq!(bash_rx.await.unwrap(), Decision::AllowOnce);
        assert!(pending.is_empty());
    }

    /// 同一工具连续两次询问：先来先答。
    #[tokio::test]
    async fn same_tool_asks_are_answered_in_order() {
        let mut pending = PendingAsks::default();
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        pending.push("bash", first_tx);
        pending.push("bash", second_tx);

        assert!(pending.resolve(&decision("bash", true, None)));
        assert_eq!(first_rx.await.unwrap(), Decision::AllowOnce);
        assert!(pending.resolve(&decision("bash", false, None)));
        assert_eq!(second_rx.await.unwrap(), Decision::Deny { reason: None });
    }

    /// 名单被丢掉（本轮结束/取消/退出）→ 通道关闭 → 内核按拒绝处理。
    #[tokio::test]
    async fn dropping_the_pending_list_denies_everything() {
        let mut pending = PendingAsks::default();
        let (tx, rx) = oneshot::channel();
        pending.push("bash", tx);
        drop(pending);
        assert_eq!(
            rx.await.unwrap_or(Decision::Deny { reason: None }),
            Decision::Deny { reason: None }
        );
    }
}
