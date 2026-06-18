mod openrouter;

use crate::images_api_registry::{register_images_api_provider, ImagesApiProvider};
use crate::images_types::*;
use std::sync::Arc;

struct OpenRouterImagesApiProvider;

impl ImagesApiProvider for OpenRouterImagesApiProvider {
    fn api(&self) -> ImagesApi {
        ImagesApi::Known(KnownImagesApi::OpenrouterImages)
    }

    fn generate_images(
        &self,
        model: ImagesModel,
        context: ImagesContext,
        options: Option<ImagesOptions>,
    ) -> ImagesFuture {
        openrouter::generate_images_openrouter(model, context, options)
    }
}

pub fn register_built_in_images_api_providers() {
    register_images_api_provider(Arc::new(OpenRouterImagesApiProvider));
}
