use crate::facades::SourceTags;
use crate::features::playback::ClientFormats;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PayloadContext {
    tags: SourceTags,
    transcoded: bool,
}

impl PayloadContext {
    pub(crate) fn new(tags: SourceTags, client: Option<&str>, formats: &ClientFormats) -> Self {
        Self {
            tags,
            transcoded: client.is_some_and(|client| formats.is_aac_client(client)),
        }
    }

    pub(crate) fn tags(self) -> SourceTags {
        self.tags
    }

    pub(crate) fn transcoded_suffix(self) -> Option<&'static str> {
        self.transcoded.then_some("aac")
    }

    pub(crate) fn transcoded_content_type(self) -> Option<&'static str> {
        self.transcoded.then_some("audio/aac")
    }
}

impl From<SourceTags> for PayloadContext {
    fn from(tags: SourceTags) -> Self {
        Self {
            tags,
            transcoded: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_aac_client_rule_sets_the_transcoded_fields() {
        let formats = ClientFormats::parse("Arpeggi=aac,Feishin=raw").unwrap();

        let arpeggi = PayloadContext::new(SourceTags::Off, Some("Arpeggi"), &formats);
        assert_eq!(arpeggi.transcoded_suffix(), Some("aac"));
        assert_eq!(arpeggi.transcoded_content_type(), Some("audio/aac"));

        let feishin = PayloadContext::new(SourceTags::Off, Some("Feishin"), &formats);
        assert_eq!(feishin.transcoded_suffix(), None);
        assert_eq!(feishin.transcoded_content_type(), None);

        let unknown = PayloadContext::from(SourceTags::ArtistPrefix);
        assert_eq!(unknown.transcoded_suffix(), None);
        assert_eq!(unknown.tags(), SourceTags::ArtistPrefix);
    }
}
