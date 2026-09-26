use reqwest::header::CONTENT_TYPE;

use crate::platform::http::FetchError;

/// Images are bounded well below the audio spool budget.
pub(crate) const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;

/// The caller validates the initial destination; the HTTP client enforces redirects.
pub(crate) async fn fetch_artwork(
    client: &reqwest::Client,
    url: &reqwest::Url,
) -> Result<(String, bytes::Bytes), FetchError> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(FetchError::Request)?;

    if !response.status().is_success() {
        return Err(FetchError::UnexpectedStatus(response.status()));
    }

    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARTWORK_BYTES)
    {
        return Err(FetchError::ResponseTooLarge);
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    let bytes = response.bytes().await.map_err(FetchError::Request)?;
    if bytes.len() as u64 > MAX_ARTWORK_BYTES {
        return Err(FetchError::ResponseTooLarge);
    }
    Ok((content_type, bytes))
}
