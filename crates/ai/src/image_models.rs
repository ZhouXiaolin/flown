use crate::images_types::{ImagesModel, KnownImagesProvider};
use once_cell::sync::Lazy;
use std::collections::HashMap;

static BUILTIN_IMAGE_MODEL_REGISTRY: Lazy<HashMap<String, HashMap<String, ImagesModel>>> =
    Lazy::new(|| {
        serde_json::from_str(include_str!("image_models.generated.json"))
            .expect("built-in image model registry JSON is valid")
    });

pub fn get_image_model(provider: &str, model_id: &str) -> Option<ImagesModel> {
    BUILTIN_IMAGE_MODEL_REGISTRY
        .get(provider)
        .and_then(|models| models.get(model_id).cloned())
}

pub fn get_image_providers() -> Vec<KnownImagesProvider> {
    let mut providers: Vec<KnownImagesProvider> = BUILTIN_IMAGE_MODEL_REGISTRY
        .keys()
        .filter_map(|provider| match provider.as_str() {
            "openrouter" => Some(KnownImagesProvider::Openrouter),
            _ => None,
        })
        .collect();
    providers.sort_by_key(|provider| provider.to_string());
    providers
}

pub fn get_image_models(provider: &str) -> Vec<ImagesModel> {
    let mut models: Vec<ImagesModel> = BUILTIN_IMAGE_MODEL_REGISTRY
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}
