use std::{sync::Arc, time::Duration};

use whio_resolver_api::apis::configuration::Configuration;

use crate::features::playback::providers::ProviderError;

pub fn resolver_client(
    base_url: reqwest::Url,
    connect_timeout: Duration,
    total_timeout: Duration,
) -> Result<Arc<Configuration>, ProviderError> {
    let http = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(total_timeout)
        .build()
        .map_err(ProviderError::request)?;
    Ok(Arc::new(Configuration {
        base_path: base_url.as_str().trim_end_matches('/').to_owned(),
        client: http,
        ..Configuration::default()
    }))
}
