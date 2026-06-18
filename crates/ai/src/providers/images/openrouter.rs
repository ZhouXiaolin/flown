use crate::error::AiError;
use crate::images_types::*;
use crate::types::{Cost, ImageContent, ProviderResponse, TextContent, Usage};
use chrono::Utc;
use reqwest::Client;
use serde_json::json;

pub fn generate_images_openrouter(
    model: ImagesModel,
    context: ImagesContext,
    options: Option<ImagesOptions>,
) -> ImagesFuture {
    Box::pin(async move {
        let mut output = AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: vec![],
            response_id: None,
            usage: None,
            stop_reason: ImagesStopReason::Stop,
            error_message: None,
            timestamp: Utc::now(),
        };

        let Some(options) = options else {
            output.stop_reason = ImagesStopReason::Error;
            output.error_message = Some(format!("No API key for provider: {}", model.provider));
            return Ok(output);
        };
        let Some(api_key) = options.api_key.clone() else {
            output.stop_reason = if options.signal.as_ref().is_some_and(|signal| signal.is_cancelled())
            {
                ImagesStopReason::Aborted
            } else {
                ImagesStopReason::Error
            };
            output.error_message = Some(format!("No API key for provider: {}", model.provider));
            return Ok(output);
        };

        let client = Client::new();
        let mut payload = build_params(&model, &context);
        if let Some(on_payload) = &options.on_payload {
            if let Some(next_payload) = on_payload(payload.clone(), model.clone()).await {
                payload = next_payload;
            }
        }

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|error| AiError::Validation(error.to_string()))?,
        );
        for (key, value) in model.headers.iter() {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|error| AiError::Validation(error.to_string()))?,
                reqwest::header::HeaderValue::from_str(value)
                    .map_err(|error| AiError::Validation(error.to_string()))?,
            );
        }
        if let Some(extra) = &options.headers {
            for (key, value) in extra {
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(key.as_bytes())
                        .map_err(|error| AiError::Validation(error.to_string()))?,
                    reqwest::header::HeaderValue::from_str(value)
                        .map_err(|error| AiError::Validation(error.to_string()))?,
                );
            }
        }

        let mut request = client
            .post(format!("{}/chat/completions", model.base_url.trim_end_matches('/')))
            .headers(headers)
            .json(&payload);
        if let Some(timeout_ms) = options.timeout_ms {
            request = request.timeout(std::time::Duration::from_millis(timeout_ms));
        }
        let response = request.send().await.map_err(AiError::from)?;
        let provider_response = ProviderResponse {
            status: response.status().as_u16(),
            headers: response
                .headers()
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (key.to_string(), value.to_string()))
                })
                .collect(),
        };
        if let Some(on_response) = &options.on_response {
            on_response(provider_response, model.clone()).await;
        }

        let body: serde_json::Value = response.json().await?;
        output.response_id = body
            .get("id")
            .and_then(|id| id.as_str())
            .map(ToOwned::to_owned);
        output.usage = parse_usage(body.get("usage"), &model);

        if let Some(choice) = body.get("choices").and_then(|choices| choices.get(0)) {
            if let Some(content) = choice
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_str())
            {
                if !content.is_empty() {
                    output.output.push(ImagesOutputContent::Text(TextContent {
                        content_type: "text".to_string(),
                        text: content.to_string(),
                        text_signature: None,
                    }));
                }
            }

            if let Some(images) = choice
                .get("message")
                .and_then(|message| message.get("images"))
                .and_then(|images| images.as_array())
            {
                for image in images {
                    let image_url = image
                        .get("image_url")
                        .and_then(|value| {
                            value
                                .as_str()
                                .map(ToOwned::to_owned)
                                .or_else(|| {
                                    value
                                        .get("url")
                                        .and_then(|url| url.as_str())
                                        .map(ToOwned::to_owned)
                                })
                        })
                        .unwrap_or_default();
                    if !image_url.starts_with("data:") {
                        continue;
                    }
                    if let Some((mime_type, data)) = parse_data_url(&image_url) {
                        output.output.push(ImagesOutputContent::Image(ImageContent {
                            content_type: "image".to_string(),
                            data: data.to_string(),
                            mime_type: mime_type.to_string(),
                        }));
                    }
                }
            }
        }

        Ok(output)
    })
}

fn build_params(model: &ImagesModel, context: &ImagesContext) -> serde_json::Value {
    let content: Vec<serde_json::Value> = context
        .input
        .iter()
        .map(|item| match item {
            ImagesInputContent::Text(text) => json!({
                "type": "text",
                "text": text.text,
            }),
            ImagesInputContent::Image(image) => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", image.mime_type, image.data),
                }
            }),
        })
        .collect();

    let modalities = if model.output.iter().any(|entry| entry == "text") {
        vec!["image", "text"]
    } else {
        vec!["image"]
    };

    json!({
        "model": model.id,
        "messages": [
            {
                "role": "user",
                "content": content,
            }
        ],
        "stream": false,
        "modalities": modalities,
    })
}

fn parse_data_url(value: &str) -> Option<(&str, &str)> {
    let data = value.strip_prefix("data:")?;
    let (mime_type, payload) = data.split_once(";base64,")?;
    Some((mime_type, payload))
}

fn parse_usage(raw_usage: Option<&serde_json::Value>, model: &ImagesModel) -> Option<Usage> {
    let raw_usage = raw_usage?;
    let prompt_tokens = raw_usage
        .get("prompt_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let completion_tokens = raw_usage
        .get("completion_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let cached_tokens = raw_usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let cache_write_tokens = raw_usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let cache_read_tokens = cached_tokens.saturating_sub(cache_write_tokens);
    let input = prompt_tokens.saturating_sub(cache_read_tokens + cache_write_tokens);

    let mut usage = Usage {
        input,
        output: completion_tokens,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        total_tokens: input + completion_tokens + cache_read_tokens + cache_write_tokens,
        cost: Cost::default(),
    };
    usage.cost = Cost {
        input: (model.cost.input / 1_000_000.0) * input as f64,
        output: (model.cost.output / 1_000_000.0) * completion_tokens as f64,
        cache_read: (model.cost.cache_read / 1_000_000.0) * cache_read_tokens as f64,
        cache_write: (model.cost.cache_write / 1_000_000.0) * cache_write_tokens as f64,
        total: 0.0,
    };
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    Some(usage)
}
