use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::StreamExt;
use seanbot_core::{
    Agent, AgentError, AgentEvent, AllowAll, Decision, PermissionHandler, PermissionRequest, Risk,
    TurnSummary, builtin_registry, config::Config,
};
use seanbot_provider::{
    ChatRequest, ChunkStream, FinishReason, Message, ModelInfo, Provider, ProviderError,
    ProviderInfo, Role, StreamChunk, ToolCall, Usage,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------- 假 Provider：按脚本逐步返回分片 ----------

enum Step {
    Chunks(Vec<Result<StreamChunk, ProviderError>>),
    /// 先给出这些分片，然后永远挂起
    Hang(Vec<StreamChunk>),
    /// stream() 本身永不返回（模拟等待首字节）
    Never,
    Fail(ProviderError),
}

struct FakeProvider {
    info: ProviderInfo,
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeProvider {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            info: ProviderInfo {
                id: "fake".into(),
                display_name: "Fake".into(),
            },
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(Vec::new())
    }

    async fn stream(&self, req: ChatRequest) -> Result<ChunkStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("脚本步骤已用完");
        match step {
            Step::Chunks(items) => Ok(futures::stream::iter(items).boxed()),
            Step::Hang(items) => Ok(futures::stream::iter(items.into_iter().map(Ok))
                .chain(futures::stream::pending())
                .boxed()),
            Step::Never => std::future::pending().await,
            Step::Fail(e) => Err(e),
        }
    }
}

struct DenyAll;

#[async_trait]
impl PermissionHandler for DenyAll {
    async fn ask(&self, _req: PermissionRequest) -> Decision {
        Decision::Deny {
            reason: Some("不允许".into()),
        }
    }
}

/// 像交互式确认那样：只读放行、改动要求确认，并记录每次拿到的风险级别。
struct StrictConfirm {
    seen: Mutex<Vec<Risk>>,
}

impl StrictConfirm {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Risk> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl PermissionHandler for StrictConfirm {
    async fn ask(&self, req: PermissionRequest) -> Decision {
        self.seen.lock().unwrap().push(req.risk);
        if req.risk == Risk::ReadOnly {
            Decision::AllowOnce
        } else {
            Decision::Deny {
                reason: Some("需要用户确认".into()),
            }
        }
    }
}

// ---------- 助手 ----------

fn text(s: &str) -> Result<StreamChunk, ProviderError> {
    Ok(StreamChunk::TextDelta(s.into()))
}

fn call(id: &str, name: &str, args: &str) -> Result<StreamChunk, ProviderError> {
    Ok(StreamChunk::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.into(),
    }))
}

fn finish(reason: FinishReason, usage: Option<Usage>) -> Result<StreamChunk, ProviderError> {
    Ok(StreamChunk::Finished { reason, usage })
}

fn reply(s: &str) -> Step {
    Step::Chunks(vec![text(s), finish(FinishReason::Stop, None)])
}

fn tool_step(mut calls: Vec<Result<StreamChunk, ProviderError>>) -> Step {
    calls.push(finish(FinishReason::ToolCalls, None));
    Step::Chunks(calls)
}

struct Harness {
    agent: Agent,
    provider: Arc<FakeProvider>,
    dir: tempfile::TempDir,
}

fn harness_with(steps: Vec<Step>, permission: Arc<dyn PermissionHandler>) -> Harness {
    harness_full(steps, Config::default(), permission)
}

/// 自定义配置与权限处理器（例如验证上限来自配置、只读动作免确认）。
fn harness_full(
    steps: Vec<Step>,
    config: Config,
    permission: Arc<dyn PermissionHandler>,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeProvider::new(steps);
    let agent = Agent::new(
        provider.clone(),
        "fake-model",
        builtin_registry(&config),
        Arc::new(config),
        permission,
        dir.path().to_path_buf(),
    );
    Harness {
        agent,
        provider,
        dir,
    }
}

fn harness(steps: Vec<Step>) -> Harness {
    harness_with(steps, Arc::new(AllowAll))
}

async fn run(
    agent: &mut Agent,
    input: &str,
    cancel: CancellationToken,
) -> (Result<TurnSummary, AgentError>, Vec<AgentEvent>) {
    let (tx, mut rx) = mpsc::channel(1024);
    let result = agent.run_turn(input.to_string(), tx, cancel).await;
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    (result, events)
}

fn cancel_after(ms: u64) -> CancellationToken {
    let token = CancellationToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        t.cancel();
    });
    token
}

/// 每个 assistant 的 tool_call 在其后都有对应的 tool 消息。
fn assert_history_consistent(history: &[Message]) {
    for (i, m) in history.iter().enumerate() {
        for c in &m.tool_calls {
            assert!(
                history[i + 1..]
                    .iter()
                    .any(|t| t.role == Role::Tool
                        && t.tool_call_id.as_deref() == Some(c.id.as_str())),
                "tool_call {} 缺少结果",
                c.id
            );
        }
    }
}

fn tool_messages(history: &[Message]) -> Vec<&str> {
    history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.as_str())
        .collect()
}

// ---------- 测试 ----------

#[tokio::test]
async fn step_limit_comes_from_config() {
    let mut cfg = Config::default();
    cfg.agent.max_steps = 1;
    let mut h = harness_full(
        vec![tool_step(vec![call(
            "c1",
            "read",
            r#"{"path":"missing.txt"}"#,
        )])],
        cfg,
        Arc::new(AllowAll),
    );
    let (result, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert!(matches!(result, Err(AgentError::StepLimit(1))));
    assert!(events.contains(&AgentEvent::Error("已达到单轮步数上限（1）".into())));
}

/// config 的只读动作（list/get）按只读处理，不需要用户确认。
#[tokio::test]
async fn config_read_only_action_skips_confirmation() {
    let confirm = StrictConfirm::new();
    let mut h = harness_with(
        vec![
            tool_step(vec![call("c1", "config", r#"{"action":"list"}"#)]),
            reply("好"),
        ],
        confirm.clone(),
    );
    let (result, _) = run(&mut h.agent, "看看配置", CancellationToken::new()).await;
    result.unwrap();
    let results = tool_messages(h.agent.history());
    assert!(results[0].contains("可修改"), "{results:?}");
    assert_eq!(confirm.seen(), [Risk::ReadOnly]);
}

/// config 的改动动作需要用户确认：被拒绝时配置文件不会被写入。
#[tokio::test]
async fn config_set_requires_confirmation() {
    let confirm = StrictConfirm::new();
    let mut h = harness_with(
        vec![
            tool_step(vec![call(
                "c1",
                "config",
                r#"{"action":"set","key":"tools.bash.max_output","value":123}"#,
            )]),
            reply("好"),
        ],
        confirm.clone(),
    );
    let (result, _) = run(&mut h.agent, "改配置", CancellationToken::new()).await;
    result.unwrap();
    let results = tool_messages(h.agent.history());
    assert!(results[0].contains("需要用户确认"), "{results:?}");
    assert_eq!(confirm.seen(), [Risk::Mutating]);
}

#[tokio::test]
async fn plain_reply() {
    let mut h = harness(vec![reply("你好")]);
    let (res, events) = run(&mut h.agent, "hi", CancellationToken::new()).await;
    let summary = res.unwrap();
    assert_eq!(summary.steps, 1);
    assert!(!summary.cancelled);
    assert_eq!(
        events,
        vec![
            AgentEvent::MessageAppended(Message::user("hi")),
            AgentEvent::ThinkingStarted,
            AgentEvent::TextDelta("你好".into()),
            AgentEvent::MessageAppended(Message::assistant("你好", None, vec![])),
            AgentEvent::TurnFinished {
                usage: None,
                steps: 1
            },
        ]
    );
    assert_eq!(
        h.agent.history(),
        &[
            Message::user("hi"),
            Message::assistant("你好", None, vec![])
        ]
    );
}

#[tokio::test]
async fn reasoning_is_forwarded_and_recorded() {
    let mut h = harness(vec![Step::Chunks(vec![
        Ok(StreamChunk::ReasoningDelta("想".into())),
        text("答"),
        finish(FinishReason::Stop, None),
    ])]);
    let (_, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert!(events.contains(&AgentEvent::ReasoningDelta("想".into())));
    assert_eq!(h.agent.history()[1].reasoning.as_deref(), Some("想"));
}

#[tokio::test]
async fn multi_step_tool_use() {
    let mut h = harness(vec![
        tool_step(vec![call("c1", "read", r#"{"path":"a.txt"}"#)]),
        reply("文件内容是 hello"),
    ]);
    std::fs::write(h.dir.path().join("a.txt"), "hello\n").unwrap();
    let (res, events) = run(&mut h.agent, "读 a.txt", CancellationToken::new()).await;
    assert_eq!(res.unwrap().steps, 2);
    assert!(events.contains(&AgentEvent::ToolStarted {
        call_id: "c1".into(),
        name: "read".into(),
        title: "a.txt".into()
    }));
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::ToolFinished { call_id, ok: true, summary, .. } if call_id == "c1" && summary == "读取 1 行")));
    let hist = h.agent.history();
    assert_eq!(hist.len(), 4);
    assert_eq!(hist[2].tool_call_id.as_deref(), Some("c1"));
    assert!(hist[2].content.contains("hello"));
    let reqs = h.provider.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].messages.last().unwrap().role, Role::Tool);
    assert_history_consistent(hist);
}

#[tokio::test]
async fn multiple_calls_in_one_step_run_in_order() {
    let mut h = harness(vec![
        tool_step(vec![
            call("c1", "read", r#"{"path":"a.txt"}"#),
            call("c2", "read", r#"{"path":"missing.txt"}"#),
        ]),
        reply("好"),
    ]);
    std::fs::write(h.dir.path().join("a.txt"), "hello\n").unwrap();
    let (_, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    let msgs = tool_messages(h.agent.history());
    assert!(msgs[0].contains("hello"));
    assert_eq!(msgs[1], "文件不存在：missing.txt");
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolFinished { call_id, ok: false, .. } if call_id == "c2")
    ));
}

#[tokio::test]
async fn tool_errors_are_returned_to_model() {
    let mut h = harness(vec![
        tool_step(vec![
            call("c1", "read", "{oops"),
            call("c2", "teleport", "{}"),
            call("c3", "bash", r#"{"command":"rm -rf /"}"#),
        ]),
        reply("好的"),
    ]);
    let (res, _) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert_eq!(res.unwrap().steps, 2); // 本轮继续到了第二步
    let msgs = tool_messages(h.agent.history());
    assert!(msgs[0].starts_with("参数不是合法 JSON"), "{}", msgs[0]);
    assert!(msgs[1].starts_with("未知工具：teleport"), "{}", msgs[1]);
    assert_eq!(msgs[2], "命令被黑名单拒绝：rm");
}

#[tokio::test]
async fn empty_arguments_treated_as_empty_object() {
    let mut h = harness(vec![tool_step(vec![call("c1", "read", "")]), reply("好")]);
    run(&mut h.agent, "q", CancellationToken::new())
        .await
        .0
        .unwrap();
    assert_eq!(tool_messages(h.agent.history()), ["缺少字符串参数 path"]);
}

#[tokio::test]
async fn permission_denial_is_returned_to_model() {
    let mut h = harness_with(
        vec![
            tool_step(vec![call("c1", "read", r#"{"path":"a.txt"}"#)]),
            reply("好"),
        ],
        Arc::new(DenyAll),
    );
    run(&mut h.agent, "q", CancellationToken::new())
        .await
        .0
        .unwrap();
    assert_eq!(
        tool_messages(h.agent.history()),
        ["用户拒绝执行该工具：不允许"]
    );
}

#[tokio::test]
async fn step_limit_stops_turn_with_consistent_history() {
    let steps = (0..3)
        .map(|i| {
            tool_step(vec![call(
                &format!("c{i}"),
                "read",
                r#"{"path":"missing.txt"}"#,
            )])
        })
        .collect();
    let Harness {
        agent,
        provider,
        dir: _dir,
    } = harness(steps);
    let mut agent = agent.with_max_steps(3);
    let (res, events) = run(&mut agent, "q", CancellationToken::new()).await;
    assert!(matches!(res, Err(AgentError::StepLimit(3))));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Error("已达到单轮步数上限（3）".into()))
    );
    assert_eq!(provider.requests().len(), 3);
    assert_history_consistent(agent.history());
}

#[tokio::test]
async fn usage_is_summed_across_steps() {
    let u1 = Usage {
        input_tokens: 10,
        output_tokens: 1,
        cache_hit_tokens: Some(8),
        cache_miss_tokens: Some(2),
    };
    let u2 = Usage {
        input_tokens: 20,
        output_tokens: 2,
        cache_hit_tokens: Some(18),
        cache_miss_tokens: Some(2),
    };
    let mut h = harness(vec![
        Step::Chunks(vec![
            call("c1", "read", r#"{"path":"missing.txt"}"#),
            finish(FinishReason::ToolCalls, Some(u1)),
        ]),
        Step::Chunks(vec![text("ok"), finish(FinishReason::Stop, Some(u2))]),
    ]);
    let (res, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    let expected = Usage {
        input_tokens: 30,
        output_tokens: 3,
        cache_hit_tokens: Some(26),
        cache_miss_tokens: Some(4),
    };
    assert_eq!(res.unwrap().usage, Some(expected));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnFinished {
            usage: Some(expected),
            steps: 2
        })
    );
}

#[tokio::test]
async fn request_prefix_is_stable_across_steps_and_turns() {
    let mut h = harness(vec![
        tool_step(vec![call("c1", "read", r#"{"path":"missing.txt"}"#)]),
        reply("一"),
        reply("二"),
    ]);
    run(&mut h.agent, "第一轮", CancellationToken::new())
        .await
        .0
        .unwrap();
    run(&mut h.agent, "第二轮", CancellationToken::new())
        .await
        .0
        .unwrap();
    let reqs = h.provider.requests();
    assert_eq!(reqs.len(), 3);
    for w in reqs.windows(2) {
        assert_eq!(w[0].system, w[1].system);
        assert_eq!(w[0].tools, w[1].tools);
        assert!(w[1].messages.starts_with(&w[0].messages), "历史必须只追加");
    }
    let names: Vec<_> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "bash",
            "bash_session",
            "config",
            "create_skill",
            "edit",
            "kb_add",
            "kb_edit",
            "kb_list",
            "kb_search",
            "kb_update",
            "perceive",
            "read",
            "search",
            "skill",
            "web_fetch",
            "web_search"
        ]
    );
}

#[tokio::test]
async fn cancel_during_stream_keeps_partial_text_and_can_continue() {
    let mut h = harness(vec![
        Step::Hang(vec![StreamChunk::TextDelta("写到".into())]),
        reply("继续"),
    ]);
    let (res, events) = run(&mut h.agent, "写点东西", cancel_after(100)).await;
    assert!(res.unwrap().cancelled);
    assert_eq!(events.last(), Some(&AgentEvent::Cancelled));
    assert_eq!(
        h.agent.history().last().unwrap(),
        &Message::assistant("写到", None, vec![])
    );
    let (res, _) = run(&mut h.agent, "继续", CancellationToken::new()).await;
    assert!(!res.unwrap().cancelled);
    assert_eq!(h.agent.history().len(), 4);
}

#[tokio::test]
async fn cancel_before_stream_opens() {
    let mut h = harness(vec![Step::Never, reply("好")]);
    let started = Instant::now();
    let (res, events) = run(&mut h.agent, "q", cancel_after(100)).await;
    assert!(res.unwrap().cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(events.last(), Some(&AgentEvent::Cancelled));
    // 没有得到任何回应的用户消息被撤回，避免下一轮出现连续两条 user 消息
    assert!(h.agent.history().is_empty());
    run(&mut h.agent, "再来", CancellationToken::new())
        .await
        .0
        .unwrap();
    assert_eq!(
        h.agent.history(),
        &[
            Message::user("再来"),
            Message::assistant("好", None, vec![])
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_during_tool_fills_all_results() {
    let mut h = harness(vec![tool_step(vec![
        call("c1", "bash", r#"{"command":"sleep 30"}"#),
        call("c2", "read", r#"{"path":"a.txt"}"#),
    ])]);
    let started = Instant::now();
    let (res, events) = run(&mut h.agent, "q", cancel_after(300)).await;
    assert!(res.unwrap().cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        tool_messages(h.agent.history()),
        ["用户已取消", "用户已取消"]
    );
    assert_history_consistent(h.agent.history());
    assert_eq!(events.last(), Some(&AgentEvent::Cancelled));
}

#[tokio::test]
async fn auth_error_before_stream() {
    let mut h = harness(vec![Step::Fail(ProviderError::Auth {
        status: 401,
        body: "bad".into(),
    })]);
    let (res, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert!(matches!(
        res,
        Err(AgentError::Provider(ProviderError::Auth { .. }))
    ));
    assert!(matches!(events.last(), Some(AgentEvent::Error(m)) if m.contains("sean config")));
    assert!(h.agent.history().is_empty());
}

#[tokio::test]
async fn mid_stream_error_keeps_partial_text() {
    let mut h = harness(vec![Step::Chunks(vec![
        text("半"),
        Err(ProviderError::Network("reset".into())),
    ])]);
    let (res, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert!(res.is_err());
    assert!(matches!(events.last(), Some(AgentEvent::Error(m)) if m.contains("reset")));
    assert_eq!(h.agent.history().last().unwrap().content, "半");
}

#[tokio::test]
async fn clear_keeps_system_prompt() {
    let mut h = harness(vec![reply("好")]);
    let before = h.agent.system_prompt().to_string();
    run(&mut h.agent, "q", CancellationToken::new())
        .await
        .0
        .unwrap();
    h.agent.clear();
    assert!(h.agent.history().is_empty());
    assert_eq!(h.agent.system_prompt(), before);
    assert!(before.contains(&h.dir.path().display().to_string()));
}

#[tokio::test]
async fn runtime_reflects_provider_and_model() {
    let mut h = harness(vec![]);
    {
        let rt = h.agent.runtime();
        let state = rt.read().unwrap();
        assert_eq!(state.provider, "fake");
        assert_eq!(state.model, "fake-model");
    }
    h.agent.set_model("other");
    assert_eq!(h.agent.runtime().read().unwrap().model, "other");
}

/// 按事件重放历史：MessageAppended → push，MessageRetracted → pop。
fn replay(events: &[AgentEvent], mut base: Vec<Message>) -> Vec<Message> {
    for e in events {
        match e {
            AgentEvent::MessageAppended(m) => base.push(m.clone()),
            AgentEvent::MessageRetracted => {
                base.pop();
            }
            _ => {}
        }
    }
    base
}

#[tokio::test]
async fn message_events_mirror_history_on_every_path() {
    // 多步工具调用
    let mut h = harness(vec![
        tool_step(vec![call("c1", "read", r#"{"path":"missing.txt"}"#)]),
        reply("好"),
    ]);
    let (_, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert_eq!(replay(&events, vec![]), h.agent.history());

    // 首步失败：用户消息被撤回
    let mut h = harness(vec![Step::Fail(ProviderError::Network("x".into()))]);
    let (_, events) = run(&mut h.agent, "q", CancellationToken::new()).await;
    assert!(events.contains(&AgentEvent::MessageRetracted));
    assert_eq!(replay(&events, vec![]), h.agent.history());
    assert!(h.agent.history().is_empty());

    // 流中途取消：部分正文保留
    let mut h = harness(vec![Step::Hang(vec![StreamChunk::TextDelta("半".into())])]);
    let (_, events) = run(&mut h.agent, "q", cancel_after(100)).await;
    assert_eq!(replay(&events, vec![]), h.agent.history());

    // 步数上限：补齐的工具结果也有事件
    let steps = (0..2)
        .map(|i| tool_step(vec![call(&format!("c{i}"), "read", r#"{"path":"m"}"#)]))
        .collect();
    let Harness { agent, .. } = harness(steps);
    let mut agent = agent.with_max_steps(2);
    let (_, events) = run(&mut agent, "q", CancellationToken::new()).await;
    assert_eq!(replay(&events, vec![]), agent.history());
}

#[tokio::test]
async fn usage_accumulates_in_runtime_across_turns() {
    let u = Usage {
        input_tokens: 10,
        output_tokens: 1,
        cache_hit_tokens: Some(8),
        cache_miss_tokens: Some(2),
    };
    let mut h = harness(vec![
        Step::Chunks(vec![text("一"), finish(FinishReason::Stop, Some(u))]),
        Step::Chunks(vec![text("二"), finish(FinishReason::Stop, Some(u))]),
    ]);
    run(&mut h.agent, "a", CancellationToken::new())
        .await
        .0
        .unwrap();
    run(&mut h.agent, "b", CancellationToken::new())
        .await
        .0
        .unwrap();
    let total = h.agent.runtime().read().unwrap().usage;
    assert_eq!(total.input_tokens, 20);
    assert_eq!(total.cache_hit_tokens, Some(16));
}

#[tokio::test]
async fn resumed_agent_sends_identical_prefix() {
    let mut first = harness(vec![
        tool_step(vec![call("c1", "read", r#"{"path":"missing.txt"}"#)]),
        reply("完成"),
    ]);
    run(&mut first.agent, "第一问", CancellationToken::new())
        .await
        .0
        .unwrap();
    let system = first.agent.system_prompt().to_string();
    let history = first.agent.history().to_vec();

    let provider = FakeProvider::new(vec![reply("继续")]);
    let mut resumed = Agent::new(
        provider.clone(),
        "fake-model",
        builtin_registry(&Config::default()),
        Arc::new(Config::default()),
        Arc::new(AllowAll),
        first.dir.path().to_path_buf(),
    )
    .with_system_prompt(system.clone())
    .with_history(history.clone());
    run(&mut resumed, "第二问", CancellationToken::new())
        .await
        .0
        .unwrap();

    let req = &provider.requests()[0];
    assert_eq!(req.system, system);
    assert!(req.messages.starts_with(&history));
    assert_eq!(req.messages.len(), history.len() + 1);
}
