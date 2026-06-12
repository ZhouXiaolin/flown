use crate::types::*;
use futures::stream::{Stream, StreamExt};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use thiserror::Error;

/// Stream of assistant message events with `.result()` support.
/// Wraps an inner stream and collects the final AssistantMessage from done/error events.
pub struct AssistantMessageEventStream {
    inner: Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>,
    final_message: Option<AssistantMessage>,
}

impl std::fmt::Debug for AssistantMessageEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantMessageEventStream")
            .field("final_message", &self.final_message)
            .finish_non_exhaustive()
    }
}

impl AssistantMessageEventStream {
    /// Create a new stream wrapping the given inner stream
    pub fn new(inner: Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>) -> Self {
        Self {
            inner,
            final_message: None,
        }
    }

    /// Consume the stream and return the final AssistantMessage.
    /// Equivalent to pi-mono's EventStream.result().
    pub async fn result(mut self) -> AssistantMessage {
        while let Some(event) = self.inner.next().await {
            match event {
                AssistantMessageEvent::Done { message, .. } => return message,
                AssistantMessageEvent::Error { error, .. } => return error,
                AssistantMessageEvent::Start { partial } => {
                    self.final_message = Some(partial);
                }
                _ => {}
            }
        }
        self.final_message.unwrap_or_else(|| AssistantMessage {
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
            timestamp: chrono::Utc::now(),
        })
    }

    /// Convert back into a raw pinned stream
    pub fn into_inner(self) -> Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>> {
        self.inner
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Stream function signature
pub type StreamFn =
    Box<dyn Fn(Model, Context, Option<StreamOptions>) -> AssistantMessageEventStream + Send + Sync>;

/// Simple stream function signature
pub type SimpleStreamFn = Box<
    dyn Fn(Model, Context, Option<SimpleStreamOptions>) -> AssistantMessageEventStream
        + Send
        + Sync,
>;

/// Raw event stream from a provider (before wrapping)
pub type RawEventStream = Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>;

/// API provider trait
pub trait ApiProvider: Send + Sync {
    fn api(&self) -> Api;
    fn stream(
        &self,
        model: &Model,
        context: &crate::types::Context,
        options: Option<&StreamOptions>,
    ) -> RawEventStream;
    fn stream_simple(
        &self,
        model: &Model,
        context: &crate::types::Context,
        options: Option<&SimpleStreamOptions>,
    ) -> RawEventStream;
}

#[derive(Debug, Error)]
pub enum AiError {
    #[error("No API provider registered for api: {api}")]
    MissingProvider { api: Api },
}

pub type Result<T> = std::result::Result<T, AiError>;

/// Global API provider registry
static API_PROVIDER_REGISTRY: RwLock<Option<HashMap<Api, Arc<dyn ApiProvider>>>> =
    RwLock::new(None);

/// Initialize the registry if needed
fn ensure_registry() {
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    if registry.is_none() {
        *registry = Some(HashMap::new());
    }
}

/// Register an API provider
pub fn register_api_provider(provider: Arc<dyn ApiProvider>) {
    ensure_registry();
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    if let Some(ref mut map) = *registry {
        map.insert(provider.api(), provider);
    }
}

/// Get an API provider by API type
pub fn get_api_provider(api: &Api) -> Option<Arc<dyn ApiProvider>> {
    ensure_registry();
    let registry = API_PROVIDER_REGISTRY.read().unwrap();
    registry.as_ref().and_then(|map| map.get(api).cloned())
}

/// Get all registered API providers
pub fn get_api_providers() -> Vec<Arc<dyn ApiProvider>> {
    ensure_registry();
    let registry = API_PROVIDER_REGISTRY.read().unwrap();
    registry
        .as_ref()
        .map(|map| map.values().cloned().collect())
        .unwrap_or_default()
}

/// Clear all registered providers
pub fn clear_api_providers() {
    let mut registry = API_PROVIDER_REGISTRY.write().unwrap();
    *registry = None;
}

/// Stream function that dispatches to the appropriate provider
pub fn stream(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&StreamOptions>,
) -> AssistantMessageEventStream {
    try_stream(model, context, options).unwrap_or_else(|error| panic!("{error}"))
}

/// Fallible stream function that dispatches to the appropriate provider.
pub fn try_stream(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&StreamOptions>,
) -> Result<AssistantMessageEventStream> {
    let provider = get_api_provider(&model.api).ok_or_else(|| AiError::MissingProvider {
        api: model.api.clone(),
    })?;
    let raw = provider.stream(model, context, options);
    Ok(AssistantMessageEventStream::new(raw))
}

/// Simple stream function with thinking level support
pub fn stream_simple(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    try_stream_simple(model, context, options).unwrap_or_else(|error| panic!("{error}"))
}

/// Fallible simple stream function with thinking level support.
pub fn try_stream_simple(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&SimpleStreamOptions>,
) -> Result<AssistantMessageEventStream> {
    let provider = get_api_provider(&model.api).ok_or_else(|| AiError::MissingProvider {
        api: model.api.clone(),
    })?;
    let raw = provider.stream_simple(model, context, options);
    Ok(AssistantMessageEventStream::new(raw))
}

/// Simple completion function that returns the final AssistantMessage
pub async fn complete_simple(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&SimpleStreamOptions>,
) -> AssistantMessage {
    stream_simple(model, context, options).result().await
}

/// Fallible simple completion function that returns the final AssistantMessage.
pub async fn try_complete_simple(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&SimpleStreamOptions>,
) -> Result<AssistantMessage> {
    Ok(try_stream_simple(model, context, options)?.result().await)
}

/// Completion function that returns the final AssistantMessage.
pub async fn complete(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&StreamOptions>,
) -> AssistantMessage {
    stream(model, context, options).result().await
}

/// Fallible completion function that returns the final AssistantMessage.
pub async fn try_complete(
    model: &Model,
    context: &crate::types::Context,
    options: Option<&StreamOptions>,
) -> Result<AssistantMessage> {
    Ok(try_stream(model, context, options)?.result().await)
}
