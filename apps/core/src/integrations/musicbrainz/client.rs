use std::{sync::Arc, time::Duration};

use reqwest::{Url, header};
use serde_json::Value;

use crate::{
    features::metadata::refresh::DocumentError,
    integrations::document_http::{self, NotFoundIs},
    platform::{self, pacing::CallLimiter},
};

const MUSICBRAINZ: &str = "https://musicbrainz.org/";
const WIKIDATA: &str = "https://www.wikidata.org/";
const COMMONS: &str = "https://commons.wikimedia.org/";
const WIKI_TIMEOUT: Duration = Duration::from_secs(20);
const PACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub enum WikipediaHosts {
    ByHost,
    Fixed(Url),
}

pub struct MusicBrainzClient {
    client: reqwest::Client,
    musicbrainz: Url,
    wikidata: Url,
    commons: Url,
    wikipedia: WikipediaHosts,
    musicbrainz_agent: String,
    wikimedia_agent: String,
    limiter: Arc<CallLimiter>,
}

impl MusicBrainzClient {
    pub fn new(config: &super::config::Config) -> Result<Self, reqwest::Error> {
        Self::with_hosts(
            Url::parse(MUSICBRAINZ).expect("MusicBrainz URL is valid"),
            Url::parse(WIKIDATA).expect("Wikidata URL is valid"),
            Url::parse(COMMONS).expect("Commons URL is valid"),
            WikipediaHosts::ByHost,
            config.user_agent.clone(),
            config.wikimedia_user_agent.clone(),
            Arc::new(CallLimiter::new(PACE)),
        )
    }

    pub(crate) fn with_hosts(
        musicbrainz: Url,
        wikidata: Url,
        commons: Url,
        wikipedia: WikipediaHosts,
        musicbrainz_agent: String,
        wikimedia_agent: String,
        limiter: Arc<CallLimiter>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(WIKI_TIMEOUT)
            .user_agent(platform::http::USER_AGENT)
            .build()?;
        Ok(Self {
            client,
            musicbrainz,
            wikidata,
            commons,
            wikipedia,
            musicbrainz_agent,
            wikimedia_agent,
            limiter,
        })
    }

    pub async fn fetch_artist(&self, mbid: &str) -> Result<Value, DocumentError> {
        let mut url = self
            .musicbrainz
            .join("ws/2/artist/")
            .map_err(|_| DocumentError::Malformed)?;
        url.path_segments_mut()
            .map_err(|_| DocumentError::Malformed)?
            .pop_if_empty()
            .push(mbid);
        self.get(
            url,
            &[("inc", "url-rels+tags+genres"), ("fmt", "json")],
            &self.musicbrainz_agent,
            NotFoundIs::SourceNotFound,
            true,
        )
        .await
    }

    pub async fn search_releases(
        &self,
        artist: &str,
        title: &str,
        limit: u16,
    ) -> Result<Value, DocumentError> {
        self.search("release", artist, title, limit).await
    }

    pub async fn search_recordings(
        &self,
        artist: &str,
        title: &str,
        limit: u16,
    ) -> Result<Value, DocumentError> {
        self.search("recording", artist, title, limit).await
    }

    pub async fn fetch_release(&self, id: &str) -> Result<Value, DocumentError> {
        self.lookup("release", "recordings+artist-credits", id)
            .await
    }

    pub async fn fetch_recording(&self, id: &str) -> Result<Value, DocumentError> {
        self.lookup("recording", "artist-credits", id).await
    }

    async fn search(
        &self,
        entity: &str,
        artist: &str,
        title: &str,
        limit: u16,
    ) -> Result<Value, DocumentError> {
        let url = self
            .musicbrainz
            .join(&format!("ws/2/{entity}"))
            .map_err(|_| DocumentError::Malformed)?;
        let query = format!(
            "{entity}:\"{}\" AND artist:\"{}\"",
            escape(title),
            escape(artist)
        );
        let limit = limit.to_string();
        self.get(
            url,
            &[("query", &query), ("limit", &limit), ("fmt", "json")],
            &self.musicbrainz_agent,
            NotFoundIs::ResolutionFailed,
            true,
        )
        .await
    }

    async fn lookup(&self, entity: &str, includes: &str, id: &str) -> Result<Value, DocumentError> {
        let mut url = self
            .musicbrainz
            .join(&format!("ws/2/{entity}/"))
            .map_err(|_| DocumentError::Malformed)?;
        url.path_segments_mut()
            .map_err(|_| DocumentError::Malformed)?
            .pop_if_empty()
            .push(id);
        self.get(
            url,
            &[("inc", includes), ("fmt", "json")],
            &self.musicbrainz_agent,
            NotFoundIs::SourceNotFound,
            true,
        )
        .await
    }

    pub async fn fetch_wikidata_entity(&self, id: &str) -> Result<Value, DocumentError> {
        let url = self
            .wikidata
            .join("w/api.php")
            .map_err(|_| DocumentError::Malformed)?;
        self.get(
            url,
            &[
                ("action", "wbgetentities"),
                ("ids", id),
                ("props", "claims|sitelinks"),
                ("format", "json"),
                ("maxlag", "5"),
            ],
            &self.wikimedia_agent,
            NotFoundIs::ResolutionFailed,
            false,
        )
        .await
    }

    pub async fn fetch_commons_imageinfo(&self, file_name: &str) -> Result<Value, DocumentError> {
        let url = self
            .commons
            .join("w/api.php")
            .map_err(|_| DocumentError::Malformed)?;
        self.get(
            url,
            &[
                ("action", "query"),
                ("prop", "imageinfo"),
                ("iiprop", "extmetadata|url"),
                ("titles", &format!("File:{file_name}")),
                ("format", "json"),
                ("maxlag", "5"),
            ],
            &self.wikimedia_agent,
            NotFoundIs::ResolutionFailed,
            false,
        )
        .await
    }

    pub async fn fetch_wikipedia_summary(
        &self,
        host: &str,
        title: &str,
    ) -> Result<Value, DocumentError> {
        let base = match &self.wikipedia {
            WikipediaHosts::ByHost => {
                Url::parse(&format!("https://{host}/")).map_err(|_| DocumentError::Malformed)?
            }
            WikipediaHosts::Fixed(url) => url.clone(),
        };
        let mut url = base
            .join("api/rest_v1/page/summary/")
            .map_err(|_| DocumentError::Malformed)?;
        url.path_segments_mut()
            .map_err(|_| DocumentError::Malformed)?
            .pop_if_empty()
            .push(&title.replace(' ', "_"));
        self.get(
            url,
            &[],
            &self.wikimedia_agent,
            NotFoundIs::ResolutionFailed,
            false,
        )
        .await
    }

    pub async fn fetch_wikipedia_extract(
        &self,
        host: &str,
        title: &str,
    ) -> Result<Value, DocumentError> {
        let base = match &self.wikipedia {
            WikipediaHosts::ByHost => {
                Url::parse(&format!("https://{host}/")).map_err(|_| DocumentError::Malformed)?
            }
            WikipediaHosts::Fixed(url) => url.clone(),
        };
        let url = base
            .join("w/api.php")
            .map_err(|_| DocumentError::Malformed)?;
        self.get(
            url,
            &[
                ("action", "query"),
                ("prop", "extracts"),
                ("explaintext", "1"),
                ("titles", title),
                ("format", "json"),
                ("maxlag", "5"),
            ],
            &self.wikimedia_agent,
            NotFoundIs::ResolutionFailed,
            false,
        )
        .await
    }

    async fn get(
        &self,
        url: Url,
        params: &[(&str, &str)],
        user_agent: &str,
        not_found: NotFoundIs,
        paced: bool,
    ) -> Result<Value, DocumentError> {
        if paced {
            self.limiter.acquire().await;
        }
        let mut request = self.client.get(url).header(header::USER_AGENT, user_agent);
        for (name, value) in params {
            request = request.query(&[(name, value)]);
        }
        let sent = request.send().await.map_err(|error| error.without_url());
        document_http::json(sent, not_found).await
    }
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
