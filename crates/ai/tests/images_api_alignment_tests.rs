use flown_ai::{
    ImagesApi, ImagesContext, ImagesInputContent, ImagesOutputContent, ImagesProvider,
    KnownImagesApi, KnownImagesProvider, clear_images_api_providers, generate_images,
    get_image_model, get_image_models, get_image_providers, get_images_api_provider,
    register_built_in_images_api_providers,
};
use std::sync::Mutex;

static IMAGES_REGISTRY_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn image_model_registry_exposes_openrouter_models() {
    let model = get_image_model("openrouter", "openrouter/auto").expect("image model");
    assert_eq!(
        model.api,
        ImagesApi::Known(KnownImagesApi::OpenrouterImages)
    );
    assert_eq!(
        model.provider,
        ImagesProvider::Known(KnownImagesProvider::Openrouter)
    );
    assert!(get_image_providers().contains(&KnownImagesProvider::Openrouter));
    assert!(
        get_image_models("openrouter")
            .iter()
            .any(|candidate| candidate.id == "openrouter/auto")
    );
}

#[test]
fn built_in_images_provider_registers() {
    let _guard = IMAGES_REGISTRY_LOCK.lock().unwrap();
    clear_images_api_providers();
    register_built_in_images_api_providers();
    assert!(get_images_api_provider(&ImagesApi::Known(KnownImagesApi::OpenrouterImages)).is_some());
}

#[test]
fn openrouter_images_provider_is_available_after_registration() {
    let _guard = IMAGES_REGISTRY_LOCK.lock().unwrap();
    clear_images_api_providers();
    register_built_in_images_api_providers();
    let provider = get_images_api_provider(&ImagesApi::Known(KnownImagesApi::OpenrouterImages));
    assert!(provider.is_some());
}

#[test]
fn clear_images_api_providers_keeps_registry_empty_until_explicit_reregister() {
    let _guard = IMAGES_REGISTRY_LOCK.lock().unwrap();
    clear_images_api_providers();
    let provider = get_images_api_provider(&ImagesApi::Known(KnownImagesApi::OpenrouterImages));
    assert!(provider.is_none());
}

#[tokio::test]
async fn generate_images_returns_error_payload_without_api_key() {
    let _guard = IMAGES_REGISTRY_LOCK.lock().unwrap();
    clear_images_api_providers();
    register_built_in_images_api_providers();
    let model = get_image_model("openrouter", "openrouter/auto").expect("image model");
    let context = ImagesContext {
        input: vec![ImagesInputContent::Text(flown_ai::TextContent {
            content_type: "text".to_string(),
            text: "draw a cat".to_string(),
            text_signature: None,
        })],
    };

    let result = generate_images(&model, &context, None)
        .await
        .expect("provider returns structured error payload");

    assert_eq!(result.stop_reason, flown_ai::ImagesStopReason::Error);
    assert!(matches!(
        result.output.as_slice(),
        [] | [ImagesOutputContent::Text(_), ..] | [ImagesOutputContent::Image(_), ..]
    ));
    assert_eq!(
        result.error_message.as_deref(),
        Some("No API key for provider: openrouter")
    );
}
