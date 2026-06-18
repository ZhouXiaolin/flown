use crate::types::*;
use flown_ai::{
    AssistantContent, AssistantMessage, AssistantMessageEvent, Context, Model, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, ThinkingLevel, Tool, ToolCall, ToolResultContent,
    ToolResultMessage, Usage, validate_tool_arguments,
};
use futures::{
    Future, FutureExt,
    channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded},
    stream::{Stream, StreamExt},
};
use std::pin::Pin;
use std::sync::Arc;

/// Event sink for agent events
pub type AgentEventSink =
    Arc<dyn Fn(AgentEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Start an agent loop with a new prompt message
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    let (tx, rx) = unbounded();
    let event_sink = create_event_sink(tx);
    let run = async move {
        let _ = run_agent_loop(prompts, context, config, event_sink, signal, stream_fn).await;
    };

    Box::pin(agent_loop_events(run, rx))
}

/// Continue an agent loop from the current context
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    let (tx, rx) = unbounded();
    let event_sink = create_event_sink(tx);
    let run = async move {
        let _ = run_agent_loop_continue(context, config, event_sink, signal, stream_fn).await;
    };

    Box::pin(agent_loop_events(run, rx))
}

/// Start an agent loop and return the messages produced by this run.
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    mut config: AgentLoopConfig,
    sink: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage> {
    let mut new_messages = prompts.clone();
    let mut current_context = AgentContext {
        messages: [context.messages, prompts.clone()].concat(),
        ..context
    };

    emit(&sink, AgentEvent::AgentStart).await;
    emit(&sink, AgentEvent::TurnStart).await;

    for prompt in &prompts {
        emit(
            &sink,
            AgentEvent::MessageStart {
                message: prompt.clone(),
            },
        )
        .await;
        emit(
            &sink,
            AgentEvent::MessageEnd {
                message: prompt.clone(),
            },
        )
        .await;
    }

    run_loop(
        &mut current_context,
        &mut new_messages,
        &mut config,
        &sink,
        signal,
        stream_fn,
    )
    .await;
    new_messages
}

/// Continue an agent loop from the current context and return messages produced by this run.
pub async fn run_agent_loop_continue(
    context: AgentContext,
    mut config: AgentLoopConfig,
    sink: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage> {
    emit(&sink, AgentEvent::AgentStart).await;

    if context.messages.is_empty() {
        let m = make_error_assistant(&config.model, "Cannot continue: no messages in context");
        emit_error_sequence(&sink, m).await;
        return Vec::new();
    }
    let last_message = context.messages.last().unwrap();
    if matches!(last_message, AgentMessage::Assistant(_)) {
        let m = make_error_assistant(
            &config.model,
            "Cannot continue from message role: assistant",
        );
        emit_error_sequence(&sink, m).await;
        return Vec::new();
    }

    let mut new_messages = Vec::new();
    let mut current_context = context;

    emit(&sink, AgentEvent::TurnStart).await;

    run_loop(
        &mut current_context,
        &mut new_messages,
        &mut config,
        &sink,
        signal,
        stream_fn,
    )
    .await;
    new_messages
}

fn create_event_sink(tx: UnboundedSender<AgentEvent>) -> AgentEventSink {
    Arc::new(move |event| {
        let mut tx = tx.clone();
        Box::pin(async move {
            let _ = tx.start_send(event);
        })
    })
}

fn agent_loop_events<F>(
    run: F,
    mut rx: UnboundedReceiver<AgentEvent>,
) -> impl Stream<Item = AgentEvent> + Send
where
    F: Future<Output = ()> + Send,
{
    async_stream::stream! {
        let run = run.fuse();
        futures::pin_mut!(run);

        loop {
            futures::select! {
                _ = run => {
                    while let Some(event) = rx.next().await {
                        yield event;
                    }
                    break;
                },
                event = rx.next().fuse() => {
                    match event {
                        Some(event) => yield event,
                        None => break,
                    }
                },
            }
        }
    }
}

async fn emit(sink: &AgentEventSink, event: AgentEvent) {
    sink(event).await;
}

/// Build an assistant message that carries a terminal error (`stop_reason:
/// Error`). Used by `run_agent_loop_continue`'s validation guards and by the
/// stream-error branch so failures surface as events rather than panics,
/// mirroring pi-mono's failure handling.
fn make_error_assistant(model: &Model, message: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some(message.to_string()),
        diagnostics: None,
        timestamp: chrono::Utc::now(),
    }
}

/// Emit the canonical failure sequence for a single error assistant message:
/// `message_start` → `message_end` → `turn_end` → `agent_end`.
async fn emit_error_sequence(sink: &AgentEventSink, m: AssistantMessage) {
    let msg = AgentMessage::Assistant(m);
    emit(
        sink,
        AgentEvent::MessageStart {
            message: msg.clone(),
        },
    )
    .await;
    emit(
        sink,
        AgentEvent::MessageEnd {
            message: msg.clone(),
        },
    )
    .await;
    emit(
        sink,
        AgentEvent::TurnEnd {
            message: msg.clone(),
            tool_results: vec![],
        },
    )
    .await;
    emit(
        sink,
        AgentEvent::AgentEnd {
            messages: vec![msg],
        },
    )
    .await;
}

async fn run_loop(
    context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: &mut AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) {
    let mut has_more_tool_calls = true;
    let mut first_turn = true;

    // Get initial steering messages
    let mut pending_messages = match &config.get_steering_messages {
        Some(f) => f().await,
        None => Vec::new(),
    };

    loop {
        while has_more_tool_calls || !pending_messages.is_empty() {
            if !first_turn {
                emit(AgentEvent::TurnStart).await;
            } else {
                first_turn = false;
            }

            // Process pending messages
            if !pending_messages.is_empty() {
                for message in pending_messages.drain(..) {
                    emit(AgentEvent::MessageStart {
                        message: message.clone(),
                    })
                    .await;
                    emit(AgentEvent::MessageEnd {
                        message: message.clone(),
                    })
                    .await;
                    context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            // Stream assistant response (updates context.messages internally)
            let message =
                stream_assistant_response(context, config, emit, signal.clone(), stream_fn.clone())
                    .await;
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(message.clone()),
                    tool_results: vec![],
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return;
            }

            // Check for tool calls
            let tool_calls: Vec<&ToolCall> = message
                .content
                .iter()
                .filter_map(|c| {
                    if let AssistantContent::ToolCall(tc) = c {
                        Some(tc)
                    } else {
                        None
                    }
                })
                .collect();

            let mut tool_results = Vec::new();
            has_more_tool_calls = false;

            if !tool_calls.is_empty() {
                let batch =
                    execute_tool_calls(context, &message, config, emit, signal.clone()).await;
                tool_results = batch.messages;
                has_more_tool_calls = !batch.terminate;

                for result in &tool_results {
                    context
                        .messages
                        .push(AgentMessage::ToolResult(result.clone()));
                    new_messages.push(AgentMessage::ToolResult(result.clone()));
                }
            }

            emit(AgentEvent::TurnEnd {
                message: AgentMessage::Assistant(message.clone()),
                tool_results: tool_results.clone(),
            })
            .await;

            // Prepare next turn
            let next_turn_context = ShouldStopAfterTurnContext {
                message: message.clone(),
                tool_results: tool_results.clone(),
                context: context.clone(),
                new_messages: new_messages.clone(),
            };

            if let Some(prepare_next_turn) = &config.prepare_next_turn {
                let prepare_ctx = PrepareNextTurnContext {
                    message: next_turn_context.message.clone(),
                    tool_results: next_turn_context.tool_results.clone(),
                    context: next_turn_context.context.clone(),
                    new_messages: next_turn_context.new_messages.clone(),
                };
                if let Some(update) = prepare_next_turn(prepare_ctx, signal.clone()).await {
                    if let Some(new_context) = update.context {
                        *context = new_context;
                    }
                    if let Some(model) = update.model {
                        config.model = model;
                    }
                    if let Some(thinking_level) = update.thinking_level {
                        config.reasoning = if thinking_level == ThinkingLevel::Off {
                            None
                        } else {
                            Some(thinking_level)
                        };
                    }
                }
            }

            // Check if should stop
            if let Some(should_stop) = &config.should_stop_after_turn {
                if should_stop(next_turn_context.clone()).await {
                    emit(AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    })
                    .await;
                    return;
                }
            }

            // Get steering messages for next iteration
            pending_messages = match &config.get_steering_messages {
                Some(f) => f().await,
                None => Vec::new(),
            };
        }

        // Check for follow-up messages
        let follow_up_messages = match &config.get_follow_up_messages {
            Some(f) => f().await,
            None => Vec::new(),
        };

        if !follow_up_messages.is_empty() {
            pending_messages = follow_up_messages;
            has_more_tool_calls = true;
            continue;
        }

        // No more messages, exit
        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
}

async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> AssistantMessage {
    // Apply context transform if configured (AgentMessage[] → AgentMessage[])
    let messages = if let Some(transform) = &config.transform_context {
        transform(context.messages.clone(), signal.clone()).await
    } else {
        context.messages.clone()
    };

    // Convert to LLM messages
    let llm_messages = (config.convert_to_llm)(messages);

    let llm_tools: Vec<Tool> = context
        .tools
        .as_ref()
        .into_iter()
        .flatten()
        .map(|t| Tool {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.parameters.clone(),
        })
        .collect();

    for tool in &llm_tools {
        tracing::info!(tool = %tool.name, description = %tool.description, "tool submitted to LLM");
    }

    let llm_context = Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: llm_messages,
        tools: (!llm_tools.is_empty()).then_some(llm_tools),
    };

    // Get API key if available
    let api_key = match &config.get_api_key {
        Some(f) => f(config.model.provider.to_string()).await,
        None => None,
    };

    // Get stream function from config or use default
    let stream_options = SimpleStreamOptions {
        base: StreamOptions {
            signal,
            api_key,
            session_id: config.session_id.clone(),
            transport: config.transport.clone(),
            max_retry_delay_ms: config.max_retry_delay_ms,
            on_payload: config.on_payload.clone(),
            on_response: config.on_response.clone(),
            ..Default::default()
        },
        reasoning: config.reasoning.clone(),
        thinking_budgets: config.thinking_budgets.clone(),
    };

    let mut event_stream = if let Some(stream_fn) = stream_fn.as_ref().or(config.stream_fn.as_ref())
    {
        stream_fn(config.model.clone(), llm_context, Some(stream_options))
    } else {
        match flown_ai::stream_simple(&config.model, &llm_context, Some(&stream_options)) {
            Ok(s) => s,
            Err(error) => {
                let message = AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![],
                    api: config.model.api.clone(),
                    provider: config.model.provider.clone(),
                    model: config.model.id.clone(),
                    response_model: None,
                    response_id: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::Error,
                    error_message: Some(error.to_string()),
                    diagnostics: None,
                    timestamp: chrono::Utc::now(),
                };
                context
                    .messages
                    .push(AgentMessage::Assistant(message.clone()));
                emit(AgentEvent::MessageStart {
                    message: AgentMessage::Assistant(message.clone()),
                })
                .await;
                emit(AgentEvent::MessageEnd {
                    message: AgentMessage::Assistant(message.clone()),
                })
                .await;
                return message;
            }
        }
    };

    let mut partial: Option<AssistantMessage> = None;
    let mut added_partial = false;

    while let Some(event) = event_stream.next().await {
        match event.clone() {
            AssistantMessageEvent::Start { partial: p } => {
                context.messages.push(AgentMessage::Assistant(p.clone()));
                added_partial = true;
                partial = Some(p.clone());
                emit(AgentEvent::MessageStart {
                    message: AgentMessage::Assistant(p),
                })
                .await;
            }
            AssistantMessageEvent::TextStart { partial: p, .. }
            | AssistantMessageEvent::TextDelta { partial: p, .. }
            | AssistantMessageEvent::TextEnd { partial: p, .. }
            | AssistantMessageEvent::ThinkingStart { partial: p, .. }
            | AssistantMessageEvent::ThinkingDelta { partial: p, .. }
            | AssistantMessageEvent::ThinkingEnd { partial: p, .. }
            | AssistantMessageEvent::ToolCallStart { partial: p, .. }
            | AssistantMessageEvent::ToolCallDelta { partial: p, .. }
            | AssistantMessageEvent::ToolCallEnd { partial: p, .. } => {
                if added_partial {
                    *context.messages.last_mut().unwrap() = AgentMessage::Assistant(p.clone());
                }
                partial = Some(p.clone());
                emit(AgentEvent::MessageUpdate {
                    message: AgentMessage::Assistant(p),
                    assistant_message_event: event,
                })
                .await;
            }
            AssistantMessageEvent::Done { message, .. } => {
                if added_partial {
                    *context.messages.last_mut().unwrap() =
                        AgentMessage::Assistant(message.clone());
                } else {
                    context
                        .messages
                        .push(AgentMessage::Assistant(message.clone()));
                }
                emit(AgentEvent::MessageEnd {
                    message: AgentMessage::Assistant(message.clone()),
                })
                .await;
                return message;
            }
            AssistantMessageEvent::Error { error, .. } => {
                if added_partial {
                    *context.messages.last_mut().unwrap() = AgentMessage::Assistant(error.clone());
                } else {
                    context
                        .messages
                        .push(AgentMessage::Assistant(error.clone()));
                }
                emit(AgentEvent::MessageEnd {
                    message: AgentMessage::Assistant(error.clone()),
                })
                .await;
                return error;
            }
        }
    }

    let message = partial.unwrap_or_else(|| AssistantMessage {
        role: "assistant".to_string(),
        content: vec![AssistantContent::Text(TextContent {
            content_type: "text".to_string(),
            text: String::new(),
            text_signature: None,
        })],
        api: config.model.api.clone(),
        provider: config.model.provider.clone(),
        model: config.model.id.clone(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some("no assistant response".to_string()),
        diagnostics: None,
        timestamp: chrono::Utc::now(),
    });

    context
        .messages
        .push(AgentMessage::Assistant(message.clone()));
    emit(AgentEvent::MessageStart {
        message: AgentMessage::Assistant(message.clone()),
    })
    .await;
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::Assistant(message.clone()),
    })
    .await;
    message
}

struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

#[derive(Clone)]
struct FinalizedToolCall {
    tool_call: ToolCall,
    result: AgentToolResult,
    is_error: bool,
}

enum PreparedToolCall {
    Immediate(FinalizedToolCall),
    Prepared {
        tool: AgentTool,
        tool_call: ToolCall,
        args: serde_json::Value,
    },
}

fn create_error_tool_result(message: impl Into<String>) -> AgentToolResult {
    AgentToolResult {
        content: vec![ToolResultContent::Text(TextContent {
            content_type: "text".to_string(),
            text: message.into(),
            text_signature: None,
        })],
        details: serde_json::json!({}),
        terminate: None,
    }
}

fn create_named_error_tool_result(name: &str, message: impl Into<String>) -> AgentToolResult {
    let message = message.into();
    let text = if name.is_empty() {
        format!("Error {message}")
    } else {
        format!("Error {name} {message}")
    };
    create_error_tool_result(text)
}

fn should_terminate_tool_batch(finalized: &[FinalizedToolCall]) -> bool {
    !finalized.is_empty()
        && finalized
            .iter()
            .all(|call| call.result.terminate == Some(true))
}

async fn emit_finalized_tool_call(
    finalized: &FinalizedToolCall,
    emit: &AgentEventSink,
) -> ToolResultMessage {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: serde_json::json!({
            "content": finalized.result.content.clone(),
            "details": finalized.result.details.clone(),
        }),
        is_error: finalized.is_error,
    })
    .await;

    let tool_result = ToolResultMessage {
        role: "toolResult".to_string(),
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: finalized.result.details.clone(),
        is_error: finalized.is_error,
        timestamp: chrono::Utc::now(),
    };

    emit(AgentEvent::MessageStart {
        message: AgentMessage::ToolResult(tool_result.clone()),
    })
    .await;
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::ToolResult(tool_result.clone()),
    })
    .await;

    tool_result
}

/// Apply tool's prepare_arguments transform if present.
/// Returns the (possibly modified) arguments.
fn prepare_tool_call_arguments(tool: &AgentTool, tool_call: &ToolCall) -> ToolCall {
    if let Some(prepare) = &tool.prepare_arguments {
        let prepared = prepare(tool_call.arguments.clone());
        ToolCall {
            content_type: tool_call.content_type.clone(),
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: prepared,
            thought_signature: tool_call.thought_signature.clone(),
        }
    } else {
        tool_call.clone()
    }
}

async fn prepare_tool_call(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> PreparedToolCall {
    let Some(tool) = context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|t| t.name == tool_call.name))
    else {
        return PreparedToolCall::Immediate(FinalizedToolCall {
            tool_call: tool_call.clone(),
            result: create_named_error_tool_result(&tool_call.name, "not found"),
            is_error: true,
        });
    };

    let prepared_call = prepare_tool_call_arguments(tool, tool_call);
    let args = match flown_ai::validate_tool_arguments(&tool.parameters, &prepared_call.arguments) {
        Ok(args) => args,
        Err(err) => {
            return PreparedToolCall::Immediate(FinalizedToolCall {
                tool_call: tool_call.clone(),
                result: create_named_error_tool_result(&tool_call.name, err.to_string()),
                is_error: true,
            });
        }
    };

    if let Some(before_hook) = &config.before_tool_call {
        let ctx = BeforeToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: tool_call.clone(),
            args: args.clone(),
            context: context.clone(),
        };
        if let Some(result) = before_hook(ctx, signal).await {
            if result.block.unwrap_or(false) {
                return PreparedToolCall::Immediate(FinalizedToolCall {
                    tool_call: tool_call.clone(),
                    result: create_named_error_tool_result(
                        &tool_call.name,
                        result
                            .reason
                            .unwrap_or_else(|| "Tool execution was blocked".to_string()),
                    ),
                    is_error: true,
                });
            }
        }
    }

    PreparedToolCall::Prepared {
        tool: tool.clone(),
        tool_call: tool_call.clone(),
        args,
    }
}

async fn execute_prepared_tool_call(
    prepared: PreparedToolCall,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
) -> FinalizedToolCall {
    match prepared {
        PreparedToolCall::Immediate(finalized) => finalized,
        PreparedToolCall::Prepared {
            tool,
            tool_call,
            args,
        } => {
            if signal.as_ref().is_some_and(|signal| signal.is_cancelled()) {
                let result = create_named_error_tool_result(&tool_call.name, "Operation aborted");
                return FinalizedToolCall {
                    tool_call,
                    result,
                    is_error: true,
                };
            }

            let update_tool_call_id = tool_call.id.clone();
            let update_tool_name = tool_call.name.clone();
            let update_args = tool_call.arguments.clone();
            let emit_update: ToolUpdateFn = {
                let emit = emit.clone();
                Arc::new(move |partial_result| {
                    let emit = emit.clone();
                    let tool_call_id = update_tool_call_id.clone();
                    let tool_name = update_tool_name.clone();
                    let args = update_args.clone();
                    Box::pin(async move {
                        emit(AgentEvent::ToolExecutionUpdate {
                            tool_call_id,
                            tool_name,
                            args,
                            partial_result,
                        })
                        .await;
                    })
                })
            };

            match (tool.execute)(tool_call.id.clone(), args, signal, Some(emit_update)).await {
                Ok(result) => FinalizedToolCall {
                    tool_call,
                    result,
                    is_error: false,
                },
                Err(error) => FinalizedToolCall {
                    result: create_named_error_tool_result(&tool_call.name, error.to_string()),
                    tool_call,
                    is_error: true,
                },
            }
        }
    }
}

async fn apply_after_tool_call(
    finalized: FinalizedToolCall,
    args: serde_json::Value,
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    config: &AgentLoopConfig,
    signal: Option<AbortSignal>,
) -> FinalizedToolCall {
    let Some(after_hook) = &config.after_tool_call else {
        return finalized;
    };

    let ctx = AfterToolCallContext {
        assistant_message: assistant_message.clone(),
        tool_call: finalized.tool_call.clone(),
        args,
        result: finalized.result.clone(),
        is_error: finalized.is_error,
        context: context.clone(),
    };

    match after_hook(ctx, signal).await {
        Some(after_result) => FinalizedToolCall {
            tool_call: finalized.tool_call,
            result: AgentToolResult {
                content: after_result
                    .content
                    .unwrap_or_else(|| finalized.result.content.clone()),
                details: after_result
                    .details
                    .unwrap_or_else(|| finalized.result.details.clone()),
                terminate: after_result.terminate.or(finalized.result.terminate),
            },
            is_error: after_result.is_error.unwrap_or(finalized.is_error),
        },
        None => finalized,
    }
}

async fn execute_tool_calls(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<AbortSignal>,
) -> ExecutedToolCallBatch {
    let tool_calls: Vec<&ToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|c| {
            if let AssistantContent::ToolCall(tc) = c {
                Some(tc)
            } else {
                None
            }
        })
        .collect();

    let has_sequential = tool_calls.iter().any(|tc| {
        context
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|t| t.name == tc.name))
            .and_then(|t| t.execution_mode.as_ref())
            .map(|m| *m == ToolExecutionMode::Sequential)
            .unwrap_or(false)
    });

    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential {
        execute_tool_calls_sequential(context, assistant_message, tool_calls, config, emit, signal)
            .await
    } else {
        execute_tool_calls_parallel(context, assistant_message, tool_calls, config, emit, signal)
            .await
    }
}

async fn execute_tool_calls_sequential(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: Vec<&ToolCall>,
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<AbortSignal>,
) -> ExecutedToolCallBatch {
    let mut messages = Vec::new();
    let mut finalized_calls = Vec::new();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        })
        .await;

        let prepared = prepare_tool_call(
            context,
            assistant_message,
            tool_call,
            config,
            signal.clone(),
        )
        .await;
        let args = match &prepared {
            PreparedToolCall::Prepared { args, .. } => args.clone(),
            PreparedToolCall::Immediate(_) => serde_json::Value::Null,
        };
        let finalized = execute_prepared_tool_call(prepared, signal.clone(), emit).await;
        let finalized = apply_after_tool_call(
            finalized,
            args,
            context,
            assistant_message,
            config,
            signal.clone(),
        )
        .await;
        messages.push(emit_finalized_tool_call(&finalized, emit).await);
        finalized_calls.push(finalized);

        if signal
            .as_ref()
            .map(|signal| signal.is_cancelled())
            .unwrap_or(false)
        {
            break;
        }
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

async fn execute_tool_calls_parallel(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: Vec<&ToolCall>,
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    signal: Option<AbortSignal>,
) -> ExecutedToolCallBatch {
    use futures::future::join_all;

    let mut prepared_calls = Vec::new();
    for tool_call in &tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        })
        .await;

        let prepared = prepare_tool_call(
            context,
            assistant_message,
            tool_call,
            config,
            signal.clone(),
        )
        .await;
        let args = match &prepared {
            PreparedToolCall::Prepared { args, .. } => args.clone(),
            PreparedToolCall::Immediate(_) => serde_json::Value::Null,
        };
        prepared_calls.push((prepared, args));
    }

    let executed = join_all(prepared_calls.into_iter().map(|(prepared, args)| {
        let signal = signal.clone();
        async move {
            (
                execute_prepared_tool_call(prepared, signal, emit).await,
                args,
            )
        }
    }))
    .await;

    let mut finalized_calls = Vec::new();
    let mut messages = Vec::new();

    for (finalized, args) in executed {
        let finalized = apply_after_tool_call(
            finalized,
            args,
            context,
            assistant_message,
            config,
            signal.clone(),
        )
        .await;
        messages.push(emit_finalized_tool_call(&finalized, emit).await);
        finalized_calls.push(finalized);
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}
