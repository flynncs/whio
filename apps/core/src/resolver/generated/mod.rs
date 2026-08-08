#![allow(unused_imports, clippy::derivable_impls, clippy::empty_docs)]

pub mod catalogue_candidate;
pub use self::catalogue_candidate::CatalogueCandidate;
pub mod catalogue_search_request;
pub use self::catalogue_search_request::CatalogueSearchRequest;
pub mod catalogue_search_response;
pub use self::catalogue_search_response::CatalogueSearchResponse;
pub mod error_code;
pub use self::error_code::ErrorCode;
pub mod error_response;
pub use self::error_response::ErrorResponse;
pub mod source_identity;
pub use self::source_identity::SourceIdentity;
