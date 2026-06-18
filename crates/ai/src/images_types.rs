use crate::types::{AbortSignal, ImageContent, ModelCost, ProviderResponse, TextContent, Usage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownImagesApi {
    #[serde(rename = "openrouter-images")]
    OpenrouterImages,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImagesApi {
    Known(KnownImagesApi),
    Custom(String),
}

impl std::fmt::Display for ImagesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImagesApi::Known(api) => write!(f, "{}", api),
            ImagesApi::Custom(api) => write!(f, "{}", api),
        }
    }
}

impl std::fmt::Display for KnownImagesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KnownImagesApi::OpenrouterImages => write!(f, "openrouter-images"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownImagesProvider {
    #[serde(rename = "openrouter")]
    Openrouter,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImagesProvider {
    Known(KnownImagesProvider),
    Custom(String),
}

impl std::fmt::Display for ImagesProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImagesProvider::Known(provider) => write!(f, "{}", provider),
            ImagesProvider::Custom(provider) => write!(f, "{}", provider),
        }
    }
}

impl std::fmt::Display for KnownImagesProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KnownImagesProvider::Openrouter => write!(f, "openrouter"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImagesInputContent {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImagesOutputContent {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImagesContext {
    pub input: Vec<ImagesInputContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantImages {
    pub api: ImagesApi,
    pub provider: ImagesProvider,
    pub model: String,
    pub output: Vec<ImagesOutputContent>,
    #[serde(
        rename = "responseId",
        alias = "response_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(rename = "stopReason", alias = "stop_reason")]
    pub stop_reason: ImagesStopReason,
    #[serde(
        rename = "errorMessage",
        alias = "error_message",
        skip_serializing_if = "Option::is_none"
    )]
    pub error_message: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagesModel {
    pub id: String,
    pub name: String,
    pub api: ImagesApi,
    pub provider: ImagesProvider,
    #[serde(rename = "baseUrl", alias = "base_url")]
    pub base_url: String,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub cost: ModelCost,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

pub type ImagesOnPayloadFn = Arc<
    dyn Fn(
            serde_json::Value,
            ImagesModel,
        )
            -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>
        + Send
        + Sync,
>;

pub type ImagesOnResponseFn = Arc<
    dyn Fn(
            ProviderResponse,
            ImagesModel,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub struct ImagesOptions {
    pub signal: Option<AbortSignal>,
    pub api_key: Option<String>,
    pub on_payload: Option<ImagesOnPayloadFn>,
    pub on_response: Option<ImagesOnResponseFn>,
    pub headers: Option<HashMap<String, String>>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl std::fmt::Debug for ImagesOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagesOptions")
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("headers", &self.headers)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("metadata", &self.metadata)
            .field("on_payload", &self.on_payload.as_ref().map(|_| "<fn>"))
            .field("on_response", &self.on_response.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl Clone for ImagesOptions {
    fn clone(&self) -> Self {
        Self {
            signal: self.signal.clone(),
            api_key: self.api_key.clone(),
            on_payload: self.on_payload.clone(),
            on_response: self.on_response.clone(),
            headers: self.headers.clone(),
            timeout_ms: self.timeout_ms,
            max_retries: self.max_retries,
            max_retry_delay_ms: self.max_retry_delay_ms,
            metadata: self.metadata.clone(),
        }
    }
}

impl Default for ImagesOptions {
    fn default() -> Self {
        Self {
            signal: None,
            api_key: None,
            on_payload: None,
            on_response: None,
            headers: None,
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: None,
            metadata: None,
        }
    }
}

pub type ProviderImagesOptions = ImagesOptions;

pub type ImagesFuture =
    Pin<Box<dyn Future<Output = crate::error::Result<AssistantImages>> + Send>>;

pub type ImagesFunction =
    Arc<dyn Fn(ImagesModel, ImagesContext, Option<ImagesOptions>) -> ImagesFuture + Send + Sync>;
