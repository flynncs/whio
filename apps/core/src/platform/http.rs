#[path = "http_error.rs"]
mod error;
pub use error::FetchError;

use std::time::Duration;

use reqwest::{
    Url,
    redirect::{Attempt, Policy},
};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const STALLED_READ_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_REDIRECTS: usize = 10;

pub fn client(redirect: Policy) -> reqwest::Client {
    client_with(redirect, CONNECT_TIMEOUT, STALLED_READ_TIMEOUT)
}

pub fn client_with(
    redirect: Policy,
    connect_timeout: Duration,
    stalled_read_timeout: Duration,
) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .read_timeout(stalled_read_timeout)
        .redirect(redirect)
        .build()
        .expect("media client builds")
}

pub fn same_origin_redirect_policy() -> Policy {
    Policy::custom(|attempt: Attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.stop();
        }

        let first = attempt.previous().first().unwrap_or(attempt.url());
        if same_origin(first, attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

pub fn host_suffix_redirect_policy(suffixes: Vec<String>) -> Policy {
    Policy::custom(move |attempt: Attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.stop();
        }

        if is_allowed_suffix_urls(attempt.url(), &suffixes) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Private and local Navidrome origins are allowed when they were registered.
pub fn is_registered_media_url(base: &Url, candidate: &Url) -> bool {
    same_origin(base, candidate)
        && base.username().is_empty()
        && base.password().is_none()
        && candidate.username().is_empty()
        && candidate.password().is_none()
}

pub fn is_allowed_artwork_url(url: &Url, suffixes: &[String]) -> bool {
    is_allowed_suffix_urls(url, suffixes)
}

pub fn is_allowed_suffix_urls(url: &Url, suffixes: &[String]) -> bool {
    suffixes
        .iter()
        .any(|suffix| is_allowed_suffix_url(url, suffix))
}

/// artwork follows redirects within its own origin and across the allowed
/// artwork hosts.
pub fn artwork_redirect_policy(suffixes: Vec<String>) -> Policy {
    Policy::custom(move |attempt: Attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.stop();
        }

        let first = attempt.previous().first().unwrap_or(attempt.url());
        if same_origin(first, attempt.url()) || is_allowed_suffix_urls(attempt.url(), &suffixes) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// `suffix` must already be lowercase; DNS names are case-insensitive.
pub fn is_allowed_suffix_url(url: &Url, suffix: &str) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            let host = host.to_ascii_lowercase();
            host == suffix
                || host
                    .strip_suffix(suffix)
                    .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
        })
}

fn same_origin(first: &Url, current: &Url) -> bool {
    first.username().is_empty()
        && first.password().is_none()
        && current.username().is_empty()
        && current.password().is_none()
        && first.scheme() == current.scheme()
        && first.host() == current.host()
        && first.port_or_known_default() == current.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    use axum::{
        Router,
        http::{StatusCode, header::LOCATION},
        response::IntoResponse,
        routing::get,
    };

    #[test]
    fn suffix_urls_require_https_matching_hosts() {
        for host in ["media.example", "rr1.media.example"] {
            let url = Url::parse(&format!("https://{host}/audio")).unwrap();
            assert!(
                is_allowed_suffix_url(&url, "media.example"),
                "accepted host should be allowed: {host}"
            );
        }

        for value in [
            "http://rr1.media.example/audio",
            "https://media.example.evil.example/audio",
            "https://evil.media.example.example/audio",
            "https://other.example/audio",
            "https://media.example@evil.example/audio",
        ] {
            let url = Url::parse(value).unwrap();
            assert!(
                !is_allowed_suffix_url(&url, "media.example"),
                "accepted disallowed URL: {value}"
            );
        }
    }

    #[test]
    fn artwork_hosts_are_restricted_to_https_suffixes() {
        let suffixes = vec!["googleusercontent.com".to_owned(), "ytimg.com".to_owned()];

        for host in [
            "googleusercontent.com",
            "lh3.googleusercontent.com",
            "i.ytimg.com",
        ] {
            let url = Url::parse(&format!("https://{host}/cover.jpg")).unwrap();
            assert!(
                is_allowed_artwork_url(&url, &suffixes),
                "accepted host: {host}"
            );
        }

        for value in [
            "http://lh3.googleusercontent.com/cover.jpg",
            "https://evil.example/cover.jpg",
            "https://googleusercontent.com.evil.example/cover.jpg",
        ] {
            let url = Url::parse(value).unwrap();
            assert!(
                !is_allowed_artwork_url(&url, &suffixes),
                "rejected url: {value}"
            );
        }
    }

    #[test]
    fn configured_suffixes_replace_the_defaults() {
        let local = vec!["127.0.0.1".to_owned()];
        let url = Url::parse("https://127.0.0.1/cover.jpg").unwrap();

        assert!(is_allowed_artwork_url(&url, &local));
        assert!(!is_allowed_artwork_url(
            &url,
            &["googleusercontent.com".to_owned(), "ytimg.com".to_owned()]
        ));
    }

    #[test]
    fn registered_media_accepts_private_same_origin_servers() {
        let base = Url::parse("http://127.0.0.1:4533/music").unwrap();
        assert!(is_registered_media_url(
            &base,
            &Url::parse("http://127.0.0.1:4533/rest/stream?id=1").unwrap()
        ));
        assert!(is_registered_media_url(
            &Url::parse("https://[::1]/music").unwrap(),
            &Url::parse("https://[::1]/rest/stream?id=1").unwrap()
        ));
    }

    #[test]
    fn registered_media_rejects_origin_changes_and_credentials() {
        let base = Url::parse("http://navidrome.example/music").unwrap();
        for value in [
            "https://navidrome.example/rest/stream?id=1",
            "http://other.example/rest/stream?id=1",
            "http://navidrome.example:4533/rest/stream?id=1",
            "http://friend:secret@navidrome.example/rest/stream?id=1",
        ] {
            assert!(!is_registered_media_url(&base, &Url::parse(value).unwrap()));
        }
    }

    #[tokio::test]
    async fn direct_redirect_policy_does_not_request_a_cross_origin_target() {
        let target_requests = Arc::new(AtomicUsize::new(0));
        let target_requests_for_handler = Arc::clone(&target_requests);
        let target_app = Router::new().route(
            "/audio",
            get(move || {
                let target_requests = Arc::clone(&target_requests_for_handler);
                async move {
                    target_requests.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::OK, "unexpected target").into_response()
                }
            }),
        );
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let target_server = tokio::spawn(async move {
            axum::serve(target_listener, target_app).await.unwrap();
        });

        let redirect_app = Router::new().route(
            "/audio",
            get(move || async move {
                (
                    StatusCode::FOUND,
                    [(LOCATION, format!("http://{target_address}/audio"))],
                    "",
                )
                    .into_response()
            }),
        );
        let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_address = redirect_listener.local_addr().unwrap();
        let redirect_server = tokio::spawn(async move {
            axum::serve(redirect_listener, redirect_app).await.unwrap();
        });

        let client = client_with(
            same_origin_redirect_policy(),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );
        let response = client
            .get(format!("http://{redirect_address}/audio"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(target_requests.load(Ordering::SeqCst), 0);

        redirect_server.abort();
        target_server.abort();
    }

    #[tokio::test]
    async fn direct_redirect_policy_bounds_same_origin_redirect_loops() {
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_handler = Arc::clone(&requests);
        let app = Router::new().route(
            "/loop",
            get(move || {
                let requests = Arc::clone(&requests_for_handler);
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::FOUND, [(LOCATION, "/loop")], "").into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = client_with(
            same_origin_redirect_policy(),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );
        let response = client
            .get(format!("http://{address}/loop"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(requests.load(Ordering::SeqCst), MAX_REDIRECTS + 1);

        server.abort();
    }
}
