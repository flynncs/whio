mod http_client;
mod policy;
mod ports;
mod range;

pub use http_client::HttpMediaFetcher;
pub use policy::{
    CONNECT_TIMEOUT, MAX_REDIRECTS, STALLED_READ_TIMEOUT, client, client_with,
    host_suffix_redirect_policy, is_allowed_suffix_url, is_registered_media_url,
    same_origin_redirect_policy,
};
pub use ports::{MediaBody, MediaFetchError, MediaFetcher, MediaInfo};
pub use range::ByteRange;
