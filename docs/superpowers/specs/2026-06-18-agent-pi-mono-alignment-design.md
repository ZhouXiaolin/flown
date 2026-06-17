# 对齐 `flown-agent` 到 `pi-mono/packages/agent` 的公共 API 语义

- 日期：2026-06-18
- 状态：设计已确认，待 spec 审阅
- 范围：`crates/agent`（+ 按需修 `crates/ai` 的最小边界）。**不动** `crates/coding-agent`。

## 背景

`crates/ai` 已对齐 `pi-mono/packages/ai`（见 `2026-06-17-ai-pi-mono-alignment-design.md`）。本设计继续把 `crates/agent` 对齐到 `pi-mono/packages/agent`。

对齐标准：**两个 crate 暴露给外部的公共 API 在语义上一一对应**，并遵循 Rust 人体工程学（`thiserror` 描述库错误、`anyhow` 仅用于应用边界、`Arc<dyn Fn>` trait object 表达回调、显式 `Result` 表达可失败操作）。凡是 pi-mono 逻辑上不存在的代码，从我们的 crate 中移除。

## 探索结论（与 pi-mono 的关键分歧）

| 维度 | pi-mono (`agent.ts`) | 我们 (`agent.rs` 现状) | 处理 |
|---|---|---|---|
| API 范式 | callback 事件：`prompt(): Promise<void>` + `subscribe(listener)` + `waitForIdle()`，失败按正常事件序列 emit errorMessage assistant message | stream/result：`prompt()->Stream`、`run()->Result<AssistantMessage>` | **重写为 callback 模型** |
| 事件生命周期 | `runWithLifecycle` 自动 emit 失败 message | 无，失败走 `Result::Err` | 补齐失败事件序列 |
| `agent_loop*` 签名 | 接受可选 `streamFn` 参数 | 不接受（只从 config 读） | 对齐为可选参数 |
| `agentLoopContinue` 错误 | stream 内 emit error 后结束 | **`panic!`** | 改为事件/Result |
| 多余 API | — | `run()`/`run_messages()`/`AgentPhase`/`phase()`/`model()`/`set_*` 中部分 | 严格移除 pi-mono 没有的 |
| 缺失 API | `subscribe`/`waitForIdle`/`signal`/`hasQueuedMessages`/`clearSteeringQueue`/`clearFollowUpQueue` | — | 补齐 |
| `AgentError` | — | 手写 `impl Error`，未用 `thiserror` | thiserror 化 |
| `prepareNextTurn` 入参 | `PrepareNextTurnContext`（含 message/toolResults/context/newMessages） | 仅 `Option<AbortSignal>` | 对齐为完整 context |
| 编译状态 | — | **6 个编译错误**（ai 接口变动未跟进） | 修复 |

事实核查：
- `coding-agent` **不直接使用 `Agent` 类**（只用 `AgentHarness`/`AgentTool`/`AgentMessage`/`AgentEvent` 等已对齐类型）。重写 `Agent` 对它无直接影响。
- `AssistantMessageEventStream` 已无 `into_inner()`；现为 `Stream` + `result(self)`（消费 self）。迭代用 `&mut` 的 `next()`，取最终 message 用 `result(self)`。
- `AiError` 已是 `thiserror::Error`，自带 `Display`。agent 侧 `String: From<AiError>` 错误是 agent 误用 `.into()`，应改 `.to_string()`。**无需改 ai。**
- `parse_streaming_json` 新签名 `(&str) -> Value`（已无 buffer 参数）。`proxy.rs` 同步调用方式即可。

## 决策汇总

1. **Agent API 范式**：对齐 pi-mono callback 模型（`prompt()->Result<(),AgentError>`、`subscribe`、`wait_for_idle`、失败 emit errorMessage 序列）。移除 stream/result 式 `run`。
2. **清理尺度**：严格移除 pi-mono 没有的 public API。
3. **state 访问**：`state()->AgentState` 快照读 + 细粒度 setter（`set_model` 等）作为 JS `state.x = y` 直接赋值在 Rust 的映射。
4. **ai 模块边界**：按需修 ai。经核查，6 个编译错误全部可在 agent 侧解决（`.to_string()`、迭代+`result()`、新 `parse_streaming_json` 签名），**预期不改 ai**；若发现确需 ai 暴露能力再回头补。
5. **listener 并发**：顺序 `await` 每个 listener，单任务驱动，listener 是 run settlement 的一部分（`agent_end` 后所有 listener 结束才算 idle）。

## 第 1 节 — 对齐判据（验收标准）

1. pi-mono `index.ts` 每个 `export` 在 `crates/agent/src/lib.rs` 有对应 `pub use`。
2. `Agent` 每个 public 方法有 pi-mono 对应方法，参数/返回/副作用语义一致。
3. 无 pi-mono 不存在的多余 public API（除 Rust 必需的 setter 映射）。
4. `cargo build -p flown-agent` 通过；`cargo run --example agent` 跑通；`cargo test -p flown-agent` 通过。
5. 错误处理用 `thiserror`（`AgentError`/`AgentToolError`/`SessionError`），无手写 `impl Error`。

## 第 2 节 — `Agent` 类重写

### 公共 API（对齐 `agent.ts` 的 `class Agent`）

```rust
pub struct Agent { /* 内部状态，见下 */ }

impl Agent {
    pub fn new(options: AgentOptions) -> Self;

    /// 订阅生命周期事件。返回 guard，drop 时取消订阅
    /// （对齐 pi-mono subscribe 返回的 unsubscribe 函数）。
    pub fn subscribe(
        &self,
        listener: AgentListener,
    ) -> Subscription;

    /// 当前状态快照（对齐 get state()）。
    pub fn state(&self) -> AgentState;

    // ── 细粒度 setter（JS 的 state.x = y 在 Rust 的映射）──
    pub fn set_model(&self, model: Model);
    pub fn set_thinking_level(&self, level: ThinkingLevel);
    pub fn set_system_prompt(&self, prompt: String);
    pub fn set_tools(&self, tools: Vec<AgentTool>);
    pub fn set_messages(&self, messages: Vec<AgentMessage>);

    // ── 队列模式 getter/setter（对齐 steeringMode/followUpMode）──
    pub fn steering_mode(&self) -> QueueMode;
    pub fn set_steering_mode(&self, mode: QueueMode);
    pub fn follow_up_mode(&self) -> QueueMode;
    pub fn set_follow_up_mode(&self, mode: QueueMode);

    // ── 队列操作 ──
    pub fn steer(&self, message: AgentMessage);
    pub fn follow_up(&self, message: AgentMessage);
    pub fn clear_steering_queue(&self);
    pub fn clear_follow_up_queue(&self);
    pub fn clear_all_queues(&self);
    pub fn has_queued_messages(&self) -> bool;

    // ── 运行控制 ──
    pub fn signal(&self) -> Option<AbortSignal>;
    pub fn abort(&self);
    pub async fn wait_for_idle(&self);
    pub fn reset(&self);

    // ── 主 API（对齐 prompt / continue）──
    pub async fn prompt(&self, input: PromptInput) -> Result<(), AgentError>;
    pub async fn continue_run(&self) -> Result<(), AgentError>;
}
```

### 辅助类型

```rust
/// 对齐 pi-mono prompt 的三个重载。
pub enum PromptInput {
    Text(String),
    TextWithImages { text: String, images: Vec<ImageContent> },
    Messages(Vec<AgentMessage>),
}

/// 事件监听器：按订阅顺序顺序 await，接收当前 run 的 abort signal。
pub type AgentListener = Arc<
    dyn Fn(AgentEvent, Option<AbortSignal>) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send + Sync,
>;

/// subscribe 返回的取消订阅 guard。Drop 时移除 listener（对齐 unsubscribe 函数）。
pub struct Subscription { /* 内部 handle */ }
impl Subscription { pub fn unsubscribe(self) {} }
impl Drop for Subscription { ... }
```

### 关键语义对齐

- **重入**：`prompt()`/`continue_run()` 在已有 active run 时返回 `Err(AgentError::AlreadyProcessing)`（对齐 pi-mono throw "Agent is already processing…"）。
- **continue_run 分支**：末消息是 assistant → 先 drain steering，有则 prompt 之；否则 drain follow-up，有则 prompt 之；都没有 → `Err(AgentError::CannotContinueFromAssistant)`。末消息非 assistant → 走 continuation。无消息 → `Err(AgentError::NoMessages)`。
- **失败路径**（对齐 `handleRunFailure`）：run 中出错时按正常事件序列 emit 一条 assistant message（`stop_reason = Aborted`（若 aborted）或 `Error`，`error_message` 非空，空 usage），顺序为 `message_start → message_end → turn_end → agent_end`。
- **run settlement**（对齐 `runWithLifecycle` + `activeRun.promise`）：用 `Arc<Notify>` 或 oneshot 表达 active run 的完成；`wait_for_idle` await 它，且只在 `agent_end` 所有 listener 顺序 await 完成后才 resolve。
- **事件处理**（对齐 `processEvents`）：每个事件先 reduce 内部 state（`message_end` push 到 messages、`tool_execution_*` 更新 pending_tool_calls、`turn_end` 记 errorMessage），再顺序 await 所有 listener。

### 内部状态（人体工程学：最小锁）

保留现有 interior mutability 模式：
- `state: Arc<RwLock<AgentState>>`（model/thinking_level/system_prompt/messages/is_streaming/streaming_message/pending_tool_calls/error_message）
- `tools: Arc<RwLock<Vec<AgentTool>>>`
- `steering_queue`/`follow_up_queue: Arc<RwLock<MessageQueue>>`
- `run_handle: Arc<RwLock<Option<RunHandle>>>`（替代原 `phase`/`run_abort`，承载 abort signal + 完成通知）
- 各种回调 `Arc<dyn Fn>` 字段（convert_to_llm/stream_fn/get_api_key/…）

**移除**：`phase: AtomicU8`、`AgentPhase`、`run_abort`（被 `run_handle` 统一）。`MessageQueue` 保留（drain 语义已对齐 `PendingMessageQueue`）。

### 移除的 pi-mono 不存在的 public API

`run()`、`run_messages()`、`AgentPhase`、`phase()`、`is_idle()`、`prompt_messages()`（stream）、`prompt()`（stream）、`continue_loop()`、`model()`、`thinking_level()`、`system_prompt()`、`tools()`。读访问统一走 `state()`。

## 第 3 节 — `agent_loop` / `run_agent_loop` 签名对齐

对齐 pi-mono：loop 函数接受**可选 `stream_fn` 参数**。

```rust
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    sink: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage>;

pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    sink: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage>;
```

`Agent::create_loop_config` 仍把 `stream_fn` 放进 config（给 run_loop 内部非首调用用），但顶层调用从参数传，与 pi-mono 一致。

### panic → 错误事件/Result

`agent_loop_continue` 现对"无消息"/"末消息 assistant"用 `panic!`。改为：
- stream 版：首事件 emit 一条 error assistant message（`message_start→message_end→turn_end→agent_end`），不 panic。
- `run_*` 版：同样走事件序列返回空 `Vec`（与 pi-mono `runLoop` 在 error stopReason 时的 `return` 一致），不 panic。

## 第 4 节 — 修复编译错误（agent 侧，预期不改 ai）

6 个错误根因与修法：

1. **`AssistantMessageEventStream::into_inner` 不存在**（`agent_loop.rs:394,397`、`harness.rs:1683`）：
   `stream_assistant_response` 改为 pi-mono 风格的两步消费——先用 `&mut` 引用 `while let Some(event) = stream.next().await { …emit… }` 迭代事件，再 `let final = stream.result().await;` 消费 self 取最终 message（`result(self)` 消费所有权，故必须先借后消费，不能颠倒）。`stream_fn(config.model, …)` 返回的 `AssistantMessageEventStream` 即 `Stream` + `result`。

2. **`String: From<AiError>` 不满足**（`agent_loop.rs:652`）：`AiError` 已是 `thiserror::Error`，把 `.into()`（期望 String）改为 `.to_string()`。

3. **`parse_streaming_json` 签名变**（`proxy.rs:402` 及 harness 调用）：现签名 `(&str) -> Value`，无 buffer 参数。调用点去掉多余参数。

4. **harness `stream_simple` 调用参数数错**（`harness.rs:1619`）：对齐 `stream_simple(&model, &context, Option<&SimpleStreamOptions>)` 现签名。

## 第 5 节 — 错误类型人体工程学

```rust
// crates/agent/src/types.rs
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("Agent is already processing a prompt. Use steer() or follow_up() to queue messages, or wait for completion.")]
    AlreadyProcessing,
    #[error("No messages to continue from")]
    NoMessages,
    #[error("Cannot continue from message role: assistant")]
    CannotContinueFromAssistant,
    #[error("{0}")]
    Other(String),
}
```

- 移除手写 `impl Display`/`impl Error`。
- 移除 `NoResponse` 变体（callback 模型不再返回 message，失败经事件序列表达）。
- `AgentToolError` 已 thiserror，保留。
- 核查 `SessionError`（`session/jsonl_storage.rs`）若手写 impl，改 thiserror。

## 第 6 节 — 类型层对齐核查（`types.rs` vs `types.ts`）

`types.rs` 已高度对齐。微调：

- **`prepare_next_turn` 入参语义缺口**：pi-mono `prepareNextTurn(context: PrepareNextTurnContext)` 传入 `{message, toolResults, context, newMessages}`；我们当前签名 `Fn(Option<AbortSignal>) -> …`。对齐为 `Fn(PrepareNextTurnContext, Option<AbortSignal>) -> …`，新增：
  ```rust
  pub struct PrepareNextTurnContext {
      pub message: AssistantMessage,
      pub tool_results: Vec<ToolResultMessage>,
      pub context: AgentContext,
      pub new_messages: Vec<AgentMessage>,
  }
  ```
  （pi-mono 的 `PrepareNextTurnContext extends ShouldStopAfterTurnContext`，Rust 用独立结构体表达同一组字段。）
- `run_loop` 内 `prepare_next_turn` 调用点同步传入完整 context。
- 其余类型（`AgentTool`/`AgentToolResult`/`BeforeToolCallContext`/`AfterToolCallContext`/`ShouldStopAfterTurnContext`/`AgentLoopTurnUpdate`/`AgentEvent`/`AgentState`/`AgentContext`）字段名/语义已对齐，仅核对。

## 第 7 节 — example 重写

`examples/agent.rs` 当前用 `run()` + stream。重写为 callback 模型：
- `Agent::new(options)`
- `agent.subscribe(Arc::new(|event, _signal| async move { /* 打印事件 */ }))`
- `agent.set_tools(vec![bash_tool])`
- `agent.prompt(PromptInput::Text("...".into())).await.unwrap()`
- `agent.wait_for_idle().await`

## 第 8 节 — lib.rs 导出对齐

核对 `lib.rs` 的 `pub use` 覆盖 pi-mono `index.ts` 全部 export：
- 新增：`PromptInput`、`AgentListener`、`Subscription`、`PrepareNextTurnContext`。
- `AgentPhase` 导出移除。
- 其余（`Agent`/`AgentOptions`/loop 函数/harness/session/skills/system_prompt/messages/proxy/utils/types）保留。

## 落地顺序

1. 修编译错误（第 4 节）→ `cargo build -p flown-agent` 通过。
2. thiserror 化 `AgentError` + 移除 `NoResponse`（第 5 节）。
3. 对齐 loop 签名 + panic→事件（第 3 节）。
4. 补 `PrepareNextTurnContext` + `prepare_next_turn` 入参（第 6 节）。
5. 重写 `Agent`（第 2 节）+ 辅助类型。
6. 重写 example（第 7 节）。
7. 对齐 `lib.rs` 导出（第 8 节）。
8. 验证：build + example + test。

## 风险

- **`run_loop` 内部用 `prepare_next_turn`/`stream_fn` 的旧调用点多**：重写 Agent 时需同步更新 `create_loop_config`。逐个核对调用点。
- **listener 顺序 await + run settlement**：需用 `Notify`/oneshot 正确表达"agent_end 后 listener 完成才 idle"，避免竞态。单任务驱动（不在 listener 回调里 spawn）以保证顺序。
- **ai 边界**：若重写中发现确需 ai 暴露新能力（如非消费版 `result`），回头补 ai；但当前判断 agent 侧可解。
