mod http_client;
mod ports;
mod range;

pub const DEFAULT_YOUTUBE_MEDIA_SUFFIX: &str = "googlevideo.com";

pub use crate::platform::http::FetchError as MediaFetchError;
pub use crate::platform::http::{
    CONNECT_TIMEOUT, MAX_REDIRECTS, STALLED_READ_TIMEOUT, client, client_with,
    host_suffix_redirect_policy, is_allowed_suffix_url, is_allowed_suffix_urls,
    is_registered_media_url, same_origin_redirect_policy,
};
pub use http_client::HttpMediaFetcher;
pub use ports::{MediaBody, MediaFetcher, MediaInfo};
pub use range::ByteRange;
