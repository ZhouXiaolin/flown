use crate::error::Result;
use crate::images_api_registry::generate_images as dispatch_generate_images;
use crate::images_types::{AssistantImages, ImagesContext, ImagesModel, ProviderImagesOptions};

pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ProviderImagesOptions>,
) -> Result<AssistantImages> {
    dispatch_generate_images(model, context, options).await
}
