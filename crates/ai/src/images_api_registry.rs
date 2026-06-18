use crate::error::{AiError, Result};
use crate::images_types::*;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub trait ImagesApiProvider: Send + Sync {
    fn api(&self) -> ImagesApi;
    fn generate_images(
        &self,
        model: ImagesModel,
        context: ImagesContext,
        options: Option<ImagesOptions>,
    ) -> ImagesFuture;
}

struct RegisteredImagesProvider {
    provider: Arc<dyn ImagesApiProvider>,
    source_id: Option<String>,
}

static IMAGES_API_PROVIDER_REGISTRY: RwLock<Option<HashMap<ImagesApi, RegisteredImagesProvider>>> =
    RwLock::new(None);
fn ensure_registry() {
    let mut registry = IMAGES_API_PROVIDER_REGISTRY.write().unwrap();
    if registry.is_none() {
        *registry = Some(HashMap::new());
    }
}

fn ensure_built_in_images_api_providers_bootstrapped() {
    let needs_bootstrap = IMAGES_API_PROVIDER_REGISTRY.read().unwrap().is_none();
    if needs_bootstrap {
        register_built_in_images_api_providers();
    }
}

pub fn register_images_api_provider(provider: Arc<dyn ImagesApiProvider>) {
    register_images_api_provider_with_source(provider, None);
}

pub(crate) fn register_images_api_provider_with_source(
    provider: Arc<dyn ImagesApiProvider>,
    source_id: Option<String>,
) {
    ensure_registry();
    let mut registry = IMAGES_API_PROVIDER_REGISTRY.write().unwrap();
    if let Some(ref mut map) = *registry {
        map.insert(
            provider.api(),
            RegisteredImagesProvider { provider, source_id },
        );
    }
}

pub fn get_images_api_provider(api: &ImagesApi) -> Option<Arc<dyn ImagesApiProvider>> {
    ensure_built_in_images_api_providers_bootstrapped();
    ensure_registry();
    let registry = IMAGES_API_PROVIDER_REGISTRY.read().unwrap();
    registry
        .as_ref()
        .and_then(|map| map.get(api).map(|entry| entry.provider.clone()))
}

pub fn get_images_api_providers() -> Vec<Arc<dyn ImagesApiProvider>> {
    ensure_built_in_images_api_providers_bootstrapped();
    ensure_registry();
    let registry = IMAGES_API_PROVIDER_REGISTRY.read().unwrap();
    registry
        .as_ref()
        .map(|map| map.values().map(|entry| entry.provider.clone()).collect())
        .unwrap_or_default()
}

pub fn unregister_images_api_providers(source_id: &str) {
    let mut registry = IMAGES_API_PROVIDER_REGISTRY.write().unwrap();
    if let Some(ref mut map) = *registry {
        map.retain(|_, entry| entry.source_id.as_deref() != Some(source_id));
    }
}

pub fn clear_images_api_providers() {
    let mut registry = IMAGES_API_PROVIDER_REGISTRY.write().unwrap();
    *registry = Some(HashMap::new());
}

pub fn register_built_in_images_api_providers() {
    crate::providers::register_built_in_images_api_providers();
}

pub fn reset_images_api_providers() {
    clear_images_api_providers();
    register_built_in_images_api_providers();
}

pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
) -> Result<AssistantImages> {
    let provider =
        get_images_api_provider(&model.api).ok_or_else(|| AiError::MissingImagesProvider {
            api: model.api.clone(),
        })?;
    let expected_api = provider.api();
    if model.api != expected_api {
        return Err(AiError::MismatchedImagesApi {
            actual: model.api.clone(),
            expected: expected_api,
        });
    }
    provider
        .generate_images(model.clone(), context.clone(), options.cloned())
        .await
}
