//! Agent 主循环：感知 → 思考 → 行动 → 观察 → 修正。

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use seanbot_provider::{
    ChatRequest, Message, Provider, ProviderError, StreamChunk, ToolCall, Usage,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{Config, SharedConfig},
    denylist::Denylist,
    event::{AgentEvent, TurnSummary},
    instruction::InjectedInstructions,
    permission::{Decision, PermissionHandler, PermissionRequest},
    prompt,
    registry::ToolRegistry,
    runtime::{RuntimeState, SharedRuntime, shared_runtime},
    tool::{ReadTracker, Risk, Tool, ToolContext, ToolOutput},
};

pub const DEFAULT_MAX_STEPS: u32 = 50;
/// 所有工具的兜底超时（bash 自身上限为 600 秒）。
const TOOL_TIMEOUT: Duration = Duration::from_secs(660);
const CANCELLED: &str = "用户已取消";
const INTERRUPTED: &str = "模型输出中断，工具未执行";

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("已达到单轮步数上限（{0}）")]
    StepLimit(u32),
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    model: String,
    tools: ToolRegistry,
    history: Vec<Message>,
    system: String,
    config: SharedConfig,
    permission: Arc<dyn PermissionHandler>,
    reads: ReadTracker,
    /// 已注入过的子目录指令文件（换会话时清空）
    instructions: InjectedInstructions,
    /// 常驻 bash 会话：程序退出前必须全部清理（Drop 里也有兜底）
    bash_sessions: crate::bash_session::BashSessions,
    denylist: Denylist,
    cwd: PathBuf,
    /// 外部通过 `with_max_steps` 指定的上限；`None` 表示用配置里的 `agent.max_steps`
    max_steps: Option<u32>,
    runtime: SharedRuntime,
}

#[derive(Default)]
struct StepOutcome {
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    error: Option<ProviderError>,
    cancelled: bool,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
        tools: ToolRegistry,
        config: Arc<Config>,
        permission: Arc<dyn PermissionHandler>,
        cwd: PathBuf,
    ) -> Self {
        let model = model.into();
        let runtime = shared_runtime(RuntimeState {
            provider: provider.info().id.clone(),
            model: model.clone(),
            ..RuntimeState::default()
        });
        Self::with_runtime(provider, model, tools, config, permission, cwd, runtime)
    }

    /// 用调用方提供的运行时状态构造。
    ///
    /// 权限处理器与 UI 要读权限模式、报告会话，就必须和 Agent 共用同一份状态，
    /// 因此先在外部建好 `runtime` 再传进来。
    pub fn with_runtime(
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
        tools: ToolRegistry,
        config: Arc<Config>,
        permission: Arc<dyn PermissionHandler>,
        cwd: PathBuf,
        runtime: SharedRuntime,
    ) -> Self {
        let system = prompt::system_prompt(&prompt::PromptEnv::detect(&cwd));
        let denylist = Denylist::new(&config.tools.bash.deny);
        let max_sessions = config.tools.bash.max_sessions.max(1);
        // 配置包成共享单元：`config` 工具写盘后同步内存，工具与步数上限立即读到新值
        let config = SharedConfig::new((*config).clone());
        let model = model.into();
        Self {
            provider,
            model,
            tools,
            history: Vec::new(),
            system,
            config,
            permission,
            reads: ReadTracker::default(),
            instructions: InjectedInstructions::default(),
            bash_sessions: crate::bash_session::BashSessions::new(max_sessions),
            denylist,
            cwd,
            max_steps: None,
            runtime,
        }
    }

    /// 恢复会话：使用会话文件中保存的系统提示词（保证请求前缀逐字节一致）。
    pub fn with_system_prompt(mut self, system: String) -> Self {
        self.system = system;
        self
    }

    /// 恢复会话：载入已有历史。
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = Some(max_steps);
        self
    }

    /// 单轮步数上限：`with_max_steps` 优先，否则读配置（`config` 工具改完立即生效）。
    fn max_steps(&self) -> u32 {
        self.max_steps
            .unwrap_or_else(|| self.config.read(|c| c.agent.max_steps))
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
        self.runtime.write().unwrap().model = self.model.clone();
    }

    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// 与工具、UI 共享的运行时状态。
    pub fn runtime(&self) -> SharedRuntime {
        self.runtime.clone()
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn system_prompt(&self) -> &str {
        &self.system
    }

    /// 换一个权限处理器（TUI 起手时把逐行确认换成界面确认）。
    pub fn set_permission(&mut self, permission: Arc<dyn PermissionHandler>) {
        self.permission = permission;
    }

    /// 关闭所有常驻 bash 会话。退出前显式调用一次，Drop 里还有兜底。
    pub fn shutdown(&self) {
        self.bash_sessions.shutdown();
    }

    /// 清空对话历史与读取记录；系统提示词保持不变。
    pub fn clear(&mut self) {
        self.history.clear();
        self.reads.clear();
        self.instructions.clear();
    }

    /// 切换到另一段会话（`/resume`、`/new`）：连同系统提示词一起替换。
    ///
    /// 恢复已有会话时必须用文件里保存的系统提示词，否则请求前缀会变，缓存全部失效。
    pub fn restore(&mut self, system: String, history: Vec<Message>) {
        self.system = system;
        self.history = history;
        self.reads.clear();
        self.instructions.clear();
    }

    pub async fn run_turn(
        &mut self,
        user_input: String,
        events: mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) -> Result<TurnSummary, AgentError> {
        self.push_message(Message::user(user_input), &events).await;
        let mut usage: Option<Usage> = None;
        let mut steps: u32 = 0;
        loop {
            if steps >= self.max_steps() {
                let err = AgentError::StepLimit(self.max_steps());
                emit(&events, AgentEvent::Error(err.to_string())).await;
                return Err(err);
            }
            steps += 1;
            emit(&events, AgentEvent::ThinkingStarted).await;

            let request = ChatRequest {
                model: self.model.clone(),
                system: self.system.clone(),
                messages: self.history.clone(),
                tools: self.tools.specs(),
                max_tokens: None,
            };
            let step = self.stream_step(request, &events, &cancel).await;
            if let Some(u) = &step.usage {
                usage.get_or_insert_with(Usage::default).add(u);
                self.runtime.write().unwrap().usage.add(u);
            }
            let calls = step.calls.clone();
            let produced = !step.text.is_empty() || !step.reasoning.is_empty() || !calls.is_empty();
            if produced {
                let reasoning = (!step.reasoning.is_empty()).then_some(step.reasoning);
                self.push_message(
                    Message::assistant(step.text, reasoning, step.calls),
                    &events,
                )
                .await;
            } else if steps == 1 && (step.cancelled || step.error.is_some()) {
                // 本轮第一步就被取消或失败且没有任何输出：撤回这条未被回应的用户消息，
                // 否则下一轮会出现连续两条 user 消息（已发出的请求前缀不受影响）
                self.history.pop();
                emit(&events, AgentEvent::MessageRetracted).await;
            }

            if step.cancelled {
                self.fill_results(&calls, CANCELLED, &events).await;
                emit(&events, AgentEvent::Cancelled).await;
                return Ok(TurnSummary {
                    usage,
                    steps,
                    cancelled: true,
                });
            }
            if let Some(err) = step.error {
                self.fill_results(&calls, INTERRUPTED, &events).await;
                emit(&events, AgentEvent::Error(describe_provider_error(&err))).await;
                return Err(err.into());
            }
            if calls.is_empty() {
                emit(&events, AgentEvent::TurnFinished { usage, steps }).await;
                return Ok(TurnSummary {
                    usage,
                    steps,
                    cancelled: false,
                });
            }

            for (i, call) in calls.iter().enumerate() {
                let content = self.execute(call, &events, &cancel).await;
                self.push_message(Message::tool(&call.id, content), &events)
                    .await;
                if cancel.is_cancelled() {
                    self.fill_results(&calls[i + 1..], CANCELLED, &events).await;
                    emit(&events, AgentEvent::Cancelled).await;
                    return Ok(TurnSummary {
                        usage,
                        steps,
                        cancelled: true,
                    });
                }
            }
        }
    }

    /// 调用一次模型并消费整个流。取消在等待首字节与流中途都会生效。
    async fn stream_step(
        &self,
        request: ChatRequest,
        events: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> StepOutcome {
        let mut out = StepOutcome::default();
        let opened = tokio::select! {
            r = self.provider.stream(request) => r,
            _ = cancel.cancelled() => {
                out.cancelled = true;
                return out;
            }
        };
        let mut stream = match opened {
            Ok(s) => s,
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        };
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                _ = cancel.cancelled() => {
                    out.cancelled = true;
                    break;
                }
            };
            match next {
                None => break,
                Some(Err(e)) => {
                    out.error = Some(e);
                    break;
                }
                Some(Ok(chunk)) => match chunk {
                    StreamChunk::ReasoningDelta(s) => {
                        out.reasoning.push_str(&s);
                        emit(events, AgentEvent::ReasoningDelta(s)).await;
                    }
                    StreamChunk::TextDelta(s) => {
                        out.text.push_str(&s);
                        emit(events, AgentEvent::TextDelta(s)).await;
                    }
                    StreamChunk::ToolCall(c) => out.calls.push(c),
                    StreamChunk::Finished { usage, .. } => out.usage = usage,
                },
            }
        }
        out
    }

    /// 执行一次工具调用并发出事件，返回交给模型的结果文本。
    async fn execute(
        &self,
        call: &ToolCall,
        events: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> String {
        let started = Instant::now();
        let tool = self.tools.get(&call.name);
        let raw = if call.arguments.trim().is_empty() {
            "{}"
        } else {
            call.arguments.as_str()
        };
        let parsed: Result<Value, serde_json::Error> = serde_json::from_str(raw);
        let title = match (&tool, &parsed) {
            (Some(t), Ok(args)) => t.title(args),
            _ => String::new(),
        };
        emit(
            events,
            AgentEvent::ToolStarted {
                call_id: call.id.clone(),
                name: call.name.clone(),
                title: title.clone(),
            },
        )
        .await;

        let outcome = self.invoke(call, tool, parsed, title, cancel).await;
        let (ok, content, summary, preview) = match outcome {
            Ok(out) => (!out.is_error, out.content, out.summary, out.preview),
            Err(message) => {
                let mut lines = message.lines().map(str::to_string);
                let summary = lines.next().unwrap_or_default();
                let preview = lines.collect();
                (false, message, summary, preview)
            }
        };
        emit(
            events,
            AgentEvent::ToolFinished {
                call_id: call.id.clone(),
                ok,
                summary,
                preview,
                elapsed: started.elapsed(),
            },
        )
        .await;
        content
    }

    async fn invoke(
        &self,
        call: &ToolCall,
        tool: Option<Arc<dyn Tool>>,
        parsed: Result<Value, serde_json::Error>,
        title: String,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, String> {
        let tool = tool.ok_or_else(|| {
            format!(
                "未知工具：{}。可用工具：{}",
                call.name,
                self.tools.names().join("、")
            )
        })?;
        let args = parsed.map_err(|e| format!("参数不是合法 JSON：{e}。请修正参数后重试"))?;
        if call.name == "bash"
            && let Some(command) = args.get("command").and_then(Value::as_str)
        {
            self.denylist.check(command)?;
            // 配置文件受保护：bash 不能读写（在确认之前就拒绝，避免打扰用户）
            if let Ok(config_path) = crate::config::config_path() {
                crate::config::bash_config_guard(command, &config_path, &self.cwd)?;
            }
            // 内置知识库也一样：只读官方内容，bash 不许动
            if let Ok(builtin) = crate::kb::builtin_dir()
                && crate::kb::command_mentions_builtin(command, &builtin, &self.cwd)
            {
                return Err(
                    "内置知识库是官方内容，bash 不能改动它；要补充自己的内容请用 kb_add / kb_edit，要更新内置知识库用 kb_update（或 sean kb update）".into(),
                );
            }
        }
        let request = PermissionRequest {
            tool: call.name.clone(),
            title,
            // 只读动作（如 config 的 list/get）按只读处理，不必让用户确认
            risk: if tool.read_only_call(&args) {
                Risk::ReadOnly
            } else {
                tool.risk()
            },
            args: args.clone(),
        };
        if let Decision::Deny { reason } = self.permission.ask(request).await {
            return Err(match reason {
                Some(r) => format!("用户拒绝执行该工具：{r}"),
                None => "用户拒绝执行该工具".to_string(),
            });
        }
        // 取消由工具通过 ctx.cancel 自行处理；在这里 select 掉工具 future 会让 bash 来不及杀进程组
        let ctx = ToolContext {
            cwd: self.cwd.clone(),
            cancel: cancel.child_token(),
            reads: self.reads.clone(),
            instructions: self.instructions.clone(),
            bash_sessions: self.bash_sessions.clone(),
            config: self.config.clone(),
            config_path: crate::config::config_path().ok(),
            kb_builtin: crate::kb::builtin_dir().ok(),
            kb_custom: crate::kb::custom_dir().ok(),
            data_dir: crate::config::data_dir().ok(),
            runtime: self.runtime.clone(),
        };
        match tokio::time::timeout(TOOL_TIMEOUT, tool.call(args, &ctx)).await {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err(format!("工具执行超时（{} 秒）", TOOL_TIMEOUT.as_secs())),
        }
    }

    async fn push_message(&mut self, message: Message, events: &mpsc::Sender<AgentEvent>) {
        self.history.push(message.clone());
        emit(events, AgentEvent::MessageAppended(message)).await;
    }

    /// 为未得到结果的工具调用补上结果，保证历史一致。
    async fn fill_results(
        &mut self,
        calls: &[ToolCall],
        text: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        for c in calls {
            self.push_message(Message::tool(&c.id, text), events).await;
        }
    }
}

async fn emit(events: &mpsc::Sender<AgentEvent>, event: AgentEvent) {
    // UI 已关闭时丢弃事件即可，不影响内核状态
    let _ = events.send(event).await;
}

fn describe_provider_error(err: &ProviderError) -> String {
    match err {
        ProviderError::Auth { .. } => format!("{err}。请运行 `sean config` 重新配置 API key"),
        ProviderError::BadRequest(_) => format!("{err}。若对话上下文过长，可用 /clear 清空后重试"),
        _ => err.to_string(),
    }
}
