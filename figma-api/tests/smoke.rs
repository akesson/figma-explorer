//! Live smoke test against the Figma REST API.
//!
//! Only compiles when the `live-tests` feature is enabled, because it
//! references symbols that don't exist until `scripts/regenerate.sh` has run.
//! Skipped at runtime when `FIGMA_TOKEN` is unset.
//!
//! Run with: `FIGMA_TOKEN=… cargo test -p figma-api --features live-tests --test smoke`

#![cfg(feature = "live-tests")]

#[tokio::test]
async fn get_me_returns_a_user() {
    let Ok(token) = std::env::var("FIGMA_TOKEN") else {
        eprintln!("FIGMA_TOKEN unset; skipping live smoke test.");
        return;
    };

    let mut config = figma_api::apis::configuration::Configuration::new();
    config.api_key = Some(figma_api::apis::configuration::ApiKey {
        prefix: None,
        key: token,
    });

    let me = figma_api::apis::users_api::get_me(&config)
        .await
        .expect("GET /v1/me should succeed with a valid token");

    assert!(
        me.id.is_some() || me.email.is_some(),
        "expected a user payload, got {me:?}"
    );
}
