pub mod audiodb;
pub mod beets;
pub(crate) mod document_http;
pub mod listenbrainz;
pub mod musicbrainz;
pub mod slskd;
pub mod ytm;

use crate::platform::config::{ConfigError, Vars};

#[derive(Debug)]
pub struct IntegrationsConfig {
    pub ytm: Option<ytm::config::Config>,
    pub musicbrainz: Option<musicbrainz::config::Config>,
    pub audiodb: Option<audiodb::config::Config>,
    pub listenbrainz_documents: Option<listenbrainz::documents_config::Config>,
    pub slskd: Option<slskd::config::Config>,
    pub beets: Option<beets::config::Config>,
}

impl IntegrationsConfig {
    pub(crate) fn from_vars(vars: &Vars) -> Result<Self, ConfigError> {
        Ok(Self {
            ytm: ytm::config::from_vars(vars)?,
            musicbrainz: musicbrainz::config::from_vars(vars)?,
            audiodb: audiodb::config::from_vars(vars)?,
            listenbrainz_documents: listenbrainz::documents_config::from_vars(vars)?,
            slskd: slskd::config::from_vars(vars)?,
            beets: beets::config::from_vars(vars)?,
        })
    }

    pub(crate) fn has_document_provider(&self) -> bool {
        self.ytm.is_some()
            || self.musicbrainz.is_some()
            || self.audiodb.is_some()
            || self.listenbrainz_documents.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_document_provider_reflects_each_optional_provider() {
        let empty = IntegrationsConfig {
            ytm: None,
            musicbrainz: None,
            audiodb: None,
            listenbrainz_documents: None,
            slskd: None,
            beets: None,
        };
        assert!(!empty.has_document_provider());
        let musicbrainz = IntegrationsConfig {
            musicbrainz: Some(musicbrainz::config::Config {
                user_agent: "ua".to_owned(),
                wikimedia_user_agent: "ua".to_owned(),
            }),
            ..empty
        };
        assert!(musicbrainz.has_document_provider());
        let ytm = IntegrationsConfig {
            ytm: Some(ytm::config::Config {
                url: reqwest::Url::parse("http://resolver.test").unwrap(),
                connect_timeout: std::time::Duration::from_secs(2),
                total_timeout: std::time::Duration::from_secs(35),
            }),
            musicbrainz: None,
            audiodb: None,
            listenbrainz_documents: None,
            slskd: None,
            beets: None,
        };
        assert!(ytm.has_document_provider());
        let audiodb = IntegrationsConfig {
            audiodb: Some(audiodb::config::Config {
                key: "key".to_owned(),
            }),
            ..IntegrationsConfig {
                ytm: None,
                musicbrainz: None,
                audiodb: None,
                listenbrainz_documents: None,
                slskd: None,
                beets: None,
            }
        };
        assert!(audiodb.has_document_provider());
        let listenbrainz = IntegrationsConfig {
            listenbrainz_documents: Some(listenbrainz::documents_config::Config {
                token: "token".to_owned(),
                algorithm: "algorithm".to_owned(),
            }),
            ..IntegrationsConfig {
                ytm: None,
                musicbrainz: None,
                audiodb: None,
                listenbrainz_documents: None,
                slskd: None,
                beets: None,
            }
        };
        assert!(listenbrainz.has_document_provider());
    }
}
