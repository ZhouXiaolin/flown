use crate::error::{AiError, Result};
use crate::types::*;
use futures::stream::{Stream, StreamExt};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

/// Backing source for an [`AssistantMessageEventStream`].
///
/// A stream is either driven by a provider's raw [`Stream`] (the common case,
/// produced by [`stream`] / [`stream_simple`]) or fed externally via
/// [`push`](AssistantMessageEventStream::push) (the extension case, produced by
/// [`create_assistant_message_event_stream`]). Both share the same completion
/// and `result()` semantics.
enum EventStreamSource {
    /// A provider-produced stream driven lazily as the consumer polls.
    Raw(Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>),
    /// Events pushed externally; drained from the queue.
    Push {
        queue: VecDeque<AssistantMessageEvent>,
        waiter: Option<std::task::Waker>,
    },
}

/// A stream of [`AssistantMessageEvent`]s that resolves to a final
/// [`AssistantMessage`].
///
/// Mirrors pi-ai's `EventStream` / `AssistantMessageEventStream`
/// (`utils/event-stream.ts`). Producers either supply a raw stream (via the
/// internal `from_raw` constructor used by [`stream`]/[`stream_simple`]) or
/// push events one at a time via [`push`](Self::push) /
/// [`end`](Self::end) (via [`create_assistant_message_event_stream`]).
/// Consumers either iterate (this implements [`Stream`]) or await
/// [`result`](Self::result) for the final message.
///
/// Completion semantics match pi-ai: a `done` or `error` event marks the
/// stream complete and seeds the final result; `end(result)` may also supply
/// an explicit final message.
pub struct AssistantMessageEventStream {
    source: EventStreamSource,
    done: bool,
    final_result: Option<AssistantMessage>,
}

impl std::fmt::Debug for AssistantMessageEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantMessageEventStream")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl AssistantMessageEventStream {
    /// Create an empty stream ready to receive events via
    /// [`push`](Self::push). Equivalent to pi-ai's
    /// `new AssistantMessageEventStream()`.
    pub fn new() -> Self {
        Self {
            source: EventStreamSource::Push {
                queue: VecDeque::new(),
                waiter: None,
            },
            done: false,
            final_result: None,
        }
    }

    /// Wrap a provider-produced raw stream. Used by built-in providers
    /// (via [`from_raw`](Self::from_raw)) and by external crates that build a
    /// boxed [`Stream`] of [`AssistantMessageEvent`]s (e.g. flown-agent's
    /// proxy/harness stream functions). Equivalent to pi-ai's
    /// `AssistantMessageEventStream` constructed from a raw event stream.
    pub fn from_stream(raw: RawEventStream) -> Self {
        Self {
            source: EventStreamSource::Raw(raw),
            done: false,
            final_result: None,
        }
    }

    /// Internal alias used by built-in providers.
    pub(crate) fn from_raw(raw: RawEventStream) -> Self {
        Self::from_stream(raw)
    }

    /// Push an event into the stream (push-source streams only).
    ///
    /// If `event` is `done` or `error`, the stream is marked complete and its
    /// final result is seeded from the carried message — matching pi-ai's
    /// `EventStream.push`, which resolves `result()` on a complete event.
    ///
    /// Panics if this stream was created from a raw provider stream rather than
    /// via [`new`](Self::new) / [`create_assistant_message_event_stream`].
    pub fn push(&mut self, event: AssistantMessageEvent) {
        if self.done {
            return;
        }
        self.observe_completion(&event);
        let EventStreamSource::Push { queue, waiter } = &mut self.source else {
            panic!("AssistantMessageEventStream::push called on a raw-backed stream");
        };
        queue.push_back(event);
        if let Some(waker) = waiter.take() {
            waker.wake();
        }
    }

    /// Terminate a push-source stream.
    ///
    /// If `result` is `Some`, it sets the final message (mirrors pi-ai's
    /// `EventStream.end(result?)`). Queued events remain consumable; once
    /// drained the stream yields `None`.
    ///
    /// Panics if this stream was created from a raw provider stream.
    pub fn end(&mut self, result: Option<AssistantMessage>) {
        self.done = true;
        if result.is_some() {
            self.final_result = result;
        }
        if let EventStreamSource::Push { waiter, .. } = &mut self.source {
            if let Some(waker) = waiter.take() {
                waker.wake();
            }
        } else {
            panic!("AssistantMessageEventStream::end called on a raw-backed stream");
        }
    }

    /// Seed `final_result` and mark `done` when a complete event is observed.
    fn observe_completion(&mut self, event: &AssistantMessageEvent) {
        match event {
            AssistantMessageEvent::Done { message, .. } => {
                self.final_result = Some(message.clone());
                self.done = true;
            }
            AssistantMessageEvent::Error { error, .. } => {
                self.final_result = Some(error.clone());
                self.done = true;
            }
            _ => {}
        }
    }

    /// Consume the stream and return the final [`AssistantMessage`].
    ///
    /// Equivalent to pi-ai's `EventStream.result()`: resolves once a
    /// `done`/`error` event (or an explicit [`end`](Self::end) with a result)
    /// has produced the final message.
    pub async fn result(mut self) -> AssistantMessage {
        while self.final_result.is_none() {
            if self.next().await.is_none() {
                break;
            }
        }
        self.final_result.unwrap_or_else(|| AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::Custom("unknown".to_string()),
            provider: Provider::Custom("unknown".to_string()),
            model: "unknown".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            diagnostics: None,
            timestamp: chrono::Utc::now(),
        })
    }
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Split the borrow so we can read `done` while matching `source`.
        let Self {
            source,
            done,
            final_result,
        } = &mut *self;

        match source {
            EventStreamSource::Raw(raw) => match raw.as_mut().poll_next(cx) {
                Poll::Ready(Some(event)) => {
                    // Seed `final_result` / `done` on a complete event.
                    match &event {
                        AssistantMessageEvent::Done { message, .. } => {
                            *final_result = Some(message.clone());
                            *done = true;
                        }
                        AssistantMessageEvent::Error { error, .. } => {
                            *final_result = Some(error.clone());
                            *done = true;
                        }
                        _ => {}
                    }
                    Poll::Ready(Some(event))
                }
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
            EventStreamSource::Push { queue, waiter } => {
                if let Some(event) = queue.pop_front() {
                    return Poll::Ready(Some(event));
                }
                if *done {
                    return Poll::Ready(None);
                }
                // Park: register the waker so a later `push`/`end` wakes us.
                *waiter = Some(cx.waker().clone());
                // Re-check after registering to avoid a lost-wakeup race.
                if let Some(event) = queue.pop_front() {
                    *waiter = None;
                    Poll::Ready(Some(event))
                } else if *done {
                    *waiter = None;
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

/// Free-function factory mirroring pi-ai's top-level
/// `createAssistantMessageEventStream()`. Returns an empty
/// [`AssistantMessageEventStream`] that extensions and callers drive
/// externally via [`push`](AssistantMessageEventStream::push) /
/// [`end`](AssistantMessageEventStream::end).
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}

/// Raw event stream from a provider (before wrapping). Used by built-in
/// providers and by external crates (e.g. flown-agent's proxy/harness) to hand
/// a futures [`Stream`] to the registry, which wraps it via
/// [`AssistantMessageEventStream::from_stream`].
pub type RawEventStream = Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>;

/// A registered API provider, mirroring pi-ai's public `ApiProvider`
/// interface (`api-registry.ts:23-27`): an `api` identifier plus `stream` and
/// `streamSimple` functions. External code that wants to plug in a custom
/// backend implements this trait and registers it via
/// [`register_api_provider`].
///
/// Implementations return an [`AssistantMessageEventStream`], matching
/// pi-ai's `StreamFunction` contract. Built-in providers build a raw
/// [`Stream`](futures::stream::Stream) internally and wrap it with
/// [`AssistantMessageEventStream::from_raw`].
pub trait ApiProvider: Send + Sync {
    /// The [`Api`] this provider serves.
    fn api(&self) -> Api;
    /// Stream completion, mirroring pi-ai's `stream`.
    fn stream(
        &self,
        model: &Model,
        context: &crate::types::Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream;
    /// Stream completion with reasoning support, mirroring pi-ai's `streamSimple`.
    fn stream_simple(
        &self,
        model: &Model,
        context: &crate::types::Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
}

/// Global API provider registry
struct RegisteredProvider {
    provider: Arc<dyn ApiProvider>,
    source_id: Option<String>,
}

static API_PROVIDER_REGISTRY: RwLock<Option<HashMap<Api, RegisteredProvider>>> = RwLock::new(None);

/// Initialize the registry if needed
fn ensure_registry() {
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    if registry.is_none() {
        *registry = Some(HashMap::new());
    }
}

/// Register an API provider (no source_id — cannot be unregistered by source).
pub fn register_api_provider(provider: Arc<dyn ApiProvider>) {
    register_api_provider_with_source(provider, None);
}

/// Register an API provider tagged with an optional `source_id` so it can
/// later be removed via [`unregister_api_providers`].
pub fn register_api_provider_with_source(
    provider: Arc<dyn ApiProvider>,
    source_id: Option<String>,
) {
    ensure_registry();
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    if let Some(ref mut map) = *registry {
        map.insert(
            provider.api(),
            RegisteredProvider {
                provider,
                source_id,
            },
        );
    }
}

/// Get an API provider by API type
pub fn get_api_provider(api: &Api) -> Option<Arc<dyn ApiProvider>> {
    ensure_registry();
    let registry = API_PROVIDER_REGISTRY.read().unwrap();
    registry
        .as_ref()
        .and_then(|map| map.get(api).map(|entry| entry.provider.clone()))
}

/// Get all registered API providers
pub fn get_api_providers() -> Vec<Arc<dyn ApiProvider>> {
    ensure_registry();
    let registry = API_PROVIDER_REGISTRY.read().unwrap();
    registry
        .as_ref()
        .map(|map| map.values().map(|entry| entry.provider.clone()).collect())
        .unwrap_or_default()
}

/// Remove every provider registered under `source_id`, mirroring pi-ai's
/// `unregisterApiProviders(sourceId)`. Providers registered without a
/// source_id (via `register_api_provider`) are left untouched.
pub fn unregister_api_providers(source_id: &str) {
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    if let Some(ref mut map) = *registry {
        map.retain(|_, entry| entry.source_id.as_deref() != Some(source_id));
    }
}

/// Clear all registered providers
pub fn clear_api_providers() {
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    *registry = None;
}

/// Register every built-in API provider, mirroring pi-ai's
/// `registerBuiltInApiProviders()` (`providers/register-builtins.ts:345`).
///
/// Unlike the TS package (which auto-registers via a side-effect import at
/// module load), Rust requires the embedder to call this explicitly — there is
/// no top-level side-effect hook.
pub fn register_built_in_api_providers() {
    crate::providers::anthropic::register_anthropic_provider();
    crate::providers::openai_completions::register_openai_completions_provider();
}

/// Clear the registry and re-register the built-in providers, mirroring
/// pi-ai's `resetApiProviders()` (`providers/register-builtins.ts:401`).
pub fn reset_api_providers() {
    clear_api_providers();
    register_built_in_api_providers();
}

/// Inject the provider's environment-variable API key into `options` when the
/// caller has not supplied one explicitly. Mirrors pi-ai's `withEnvApiKey`
/// (`stream.ts:22-30`): an explicit, non-empty `options.apiKey` wins; otherwise
/// the first env var for `model.provider` (see [`crate::env_api_keys`]) is used.
///
/// Returns `None` only when both the explicit key and the env key are absent *and*
/// the caller passed `None` — i.e. no options object is needed.
fn with_env_api_key(model: &Model, options: Option<&StreamOptions>) -> Option<StreamOptions> {
    let mut merged = options.cloned().unwrap_or_default();
    let has_explicit = merged
        .api_key
        .as_deref()
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false);
    if !has_explicit {
        if let Some(key) = crate::env_api_keys::get_env_api_key(&model.provider) {
            merged.api_key = Some(key);
        }
    }
    Some(merged)
}

/// Stream function that dispatches to the appropriate provider
/// Stream function that dispatches to the appropriate provider.
///
/// Returns [`Result`] rather than panicking: in pi-ai a missing provider
/// throws (`stream.ts:32-38`), and the Rust-idiomatic translation of `throw`
/// is a `Result` the caller propagates with `?`. There is no panicking
/// overload — callers that want the old behaviour can `.expect(...)`.
pub fn stream(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&StreamOptions>,
) -> Result<AssistantMessageEventStream> {
    let provider = get_api_provider(&model.api).ok_or_else(|| AiError::MissingProvider {
        api: model.api.clone(),
    })?;
    let merged = with_env_api_key(model, options);
    Ok(provider.stream(model, context, merged.as_ref()))
}

/// Simple stream function with thinking level support.
///
/// See [`stream`] for the `Result`-vs-`throw` rationale.
pub fn stream_simple(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream> {
    let provider = get_api_provider(&model.api).ok_or_else(|| AiError::MissingProvider {
        api: model.api.clone(),
    })?;
    let mut merged = options.cloned().unwrap_or_default();
    merged.base = with_env_api_key(model, Some(&merged.base)).unwrap_or_default();
    Ok(provider.stream_simple(model, context, Some(&merged)))
}

/// Simple completion function that returns the final AssistantMessage.
///
/// See [`stream`] for the `Result`-vs-`throw` rationale.
pub async fn complete_simple(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&SimpleStreamOptions>,
) -> Result<AssistantMessage> {
    Ok(stream_simple(model, context, options)?.result().await)
}

/// Completion function that returns the final AssistantMessage.
///
/// See [`stream`] for the `Result`-vs-`throw` rationale.
pub async fn complete(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&StreamOptions>,
) -> Result<AssistantMessage> {
    Ok(stream(model, context, options)?.result().await)
}
