use std::collections::HashMap;

pub const DEFAULT_ENCODE_BITRATE_KBPS: u32 = 256;
pub const MIN_ENCODE_BITRATE_KBPS: u32 = 32;
const FALLBACK_COPY_BITRATE_KBPS: u32 = 128;
const AAC_FRAME_SAMPLES: f64 = 1024.0;
const AAC_SAMPLE_RATE_HZ: f64 = 44_100.0;
const ADTS_FRAME_OVERHEAD_BYTES: f64 = 7.0;
const ESTIMATE_HEADROOM: f64 = 1.02;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientFormat {
    Aac,
    Raw,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientFormats {
    rules: HashMap<String, ClientFormat>,
}

impl ClientFormats {
    pub fn parse(raw: &str) -> Option<Self> {
        let mut rules = HashMap::new();
        for entry in raw
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let (name, format) = entry.split_once('=')?;
            let name = name.trim();
            let format = match format.trim().to_ascii_lowercase().as_str() {
                "aac" => ClientFormat::Aac,
                "raw" => ClientFormat::Raw,
                _ => return None,
            };
            if name.is_empty() {
                return None;
            }
            rules.insert(name.to_owned(), format);
        }
        Some(Self { rules })
    }

    pub fn format_for(&self, client: &str) -> Option<ClientFormat> {
        self.rules.get(client).copied()
    }

    pub fn is_aac_client(&self, client: &str) -> bool {
        self.format_for(client) == Some(ClientFormat::Aac)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceAudio {
    pub codec: Option<String>,
    pub bitrate_kbps: Option<f64>,
    pub lossless: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamRequest {
    pub format: Option<String>,
    pub max_bitrate_kbps: Option<u32>,
    pub client: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AacMode {
    Copy,
    Encode { bitrate_kbps: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Representation {
    Original,
    Aac(AacMode),
}

impl Representation {
    pub fn resolve(request: &StreamRequest, formats: &ClientFormats, source: &SourceAudio) -> Self {
        if request
            .format
            .as_deref()
            .is_some_and(|format| format.eq_ignore_ascii_case("raw"))
        {
            return Self::Original;
        }

        let max_bitrate = request.max_bitrate_kbps.filter(|kbps| *kbps > 0);
        let aac_requested = request.format.as_deref().is_some_and(|format| {
            format.eq_ignore_ascii_case("aac") || format.eq_ignore_ascii_case("m4a")
        });

        if aac_requested || over_bitrate(max_bitrate, source) {
            return Self::Aac(aac_mode(source, max_bitrate));
        }

        let client_aac = request
            .client
            .as_deref()
            .and_then(|client| formats.format_for(client))
            == Some(ClientFormat::Aac);
        if client_aac {
            return Self::Aac(aac_mode(source, max_bitrate));
        }

        Self::Original
    }

    pub fn content_type(self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aac(_) => Some("audio/aac"),
        }
    }

    pub fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aac(_) => Some("aac"),
        }
    }

    pub fn estimate_bytes(self, duration_ms: u64, source_bitrate_kbps: Option<f64>) -> u64 {
        let bitrate_kbps = match self {
            Self::Original => return 0,
            Self::Aac(AacMode::Copy) => {
                source_bitrate_kbps.unwrap_or(f64::from(FALLBACK_COPY_BITRATE_KBPS))
            }
            Self::Aac(AacMode::Encode { bitrate_kbps }) => f64::from(bitrate_kbps),
        };
        estimate_content_length_f64(duration_ms, bitrate_kbps)
    }

    pub fn is_converted(self) -> bool {
        matches!(self, Self::Aac(_))
    }

    pub fn cannot_convert(request: &StreamRequest, formats: &ClientFormats) -> bool {
        let format = request.format.as_deref();
        if format.is_some_and(|format| format.eq_ignore_ascii_case("raw")) {
            return true;
        }
        if format.is_some_and(|format| {
            format.eq_ignore_ascii_case("aac") || format.eq_ignore_ascii_case("m4a")
        }) {
            return false;
        }
        if request.max_bitrate_kbps.is_some_and(|kbps| kbps > 0) {
            return false;
        }
        request
            .client
            .as_deref()
            .and_then(|client| formats.format_for(client))
            != Some(ClientFormat::Aac)
    }
}

fn over_bitrate(max_bitrate: Option<u32>, source: &SourceAudio) -> bool {
    max_bitrate.is_some_and(|max| {
        source.lossless
            || source
                .bitrate_kbps
                .is_none_or(|source_bitrate| source_bitrate > f64::from(max))
    })
}

fn aac_mode(source: &SourceAudio, max_bitrate: Option<u32>) -> AacMode {
    let copy = source.codec.as_deref().is_some_and(is_aac_codec)
        && !max_bitrate.is_some_and(|max| {
            source
                .bitrate_kbps
                .is_some_and(|source| source > f64::from(max))
        });
    if copy {
        return AacMode::Copy;
    }
    AacMode::Encode {
        bitrate_kbps: encode_bitrate(max_bitrate),
    }
}

fn encode_bitrate(max_bitrate: Option<u32>) -> u32 {
    max_bitrate
        .unwrap_or(DEFAULT_ENCODE_BITRATE_KBPS)
        .clamp(MIN_ENCODE_BITRATE_KBPS, DEFAULT_ENCODE_BITRATE_KBPS)
}

pub fn is_aac_codec(codec: &str) -> bool {
    let codec = codec.to_ascii_lowercase();
    codec == "aac" || codec.starts_with("mp4a")
}

pub fn is_lossless_codec(codec: &str) -> bool {
    let codec = codec.to_ascii_lowercase();
    codec.starts_with("pcm")
        || matches!(
            codec.as_str(),
            "flac" | "alac" | "wavpack" | "ape" | "monkey's audio"
        )
}

pub fn is_lossless_content_type(content_type: &str) -> bool {
    matches!(
        content_type.to_ascii_lowercase().as_str(),
        "audio/flac"
            | "audio/x-flac"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/aiff"
            | "audio/x-aiff"
            | "audio/x-ape"
            | "audio/x-wavpack"
    )
}

pub fn estimate_content_length(duration_ms: u64, bitrate_kbps: u32) -> u64 {
    estimate_content_length_f64(duration_ms, f64::from(bitrate_kbps))
}

fn estimate_content_length_f64(duration_ms: u64, bitrate_kbps: f64) -> u64 {
    let duration_s = duration_ms as f64 / 1000.0;
    let audio_bytes = (duration_s * bitrate_kbps * 125.0).ceil();
    let frame_bytes =
        ADTS_FRAME_OVERHEAD_BYTES * (duration_s * AAC_SAMPLE_RATE_HZ / AAC_FRAME_SAMPLES).ceil();
    ((audio_bytes + frame_bytes) * ESTIMATE_HEADROOM).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_request(
        format: Option<&str>,
        max_bitrate: Option<u32>,
        client: Option<&str>,
    ) -> StreamRequest {
        StreamRequest {
            format: format.map(str::to_owned),
            max_bitrate_kbps: max_bitrate,
            client: client.map(str::to_owned),
        }
    }

    fn aac_source(bitrate: Option<f64>) -> SourceAudio {
        SourceAudio {
            codec: Some("mp4a.40.2".to_owned()),
            bitrate_kbps: bitrate,
            lossless: false,
        }
    }

    fn flac_source() -> SourceAudio {
        SourceAudio {
            codec: Some("flac".to_owned()),
            bitrate_kbps: Some(900.0),
            lossless: true,
        }
    }

    fn empty_rules() -> ClientFormats {
        ClientFormats::default()
    }

    fn rules(raw: &str) -> ClientFormats {
        ClientFormats::parse(raw).unwrap()
    }

    #[test]
    fn raw_format_always_keeps_the_original() {
        let request = stream_request(Some("raw"), Some(64), Some("Arpeggi"));
        let formats = rules("Arpeggi=aac");
        assert_eq!(
            Representation::resolve(&request, &formats, &flac_source()),
            Representation::Original
        );
    }

    #[test]
    fn requested_aac_encodes_or_copies() {
        let request = stream_request(Some("aac"), None, None);
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &flac_source()),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 256 })
        );
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &aac_source(Some(192.0))),
            Representation::Aac(AacMode::Copy)
        );

        let m4a = stream_request(Some("M4A"), None, None);
        assert_eq!(
            Representation::resolve(&m4a, &empty_rules(), &flac_source()),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 256 })
        );
    }

    #[test]
    fn a_max_bitrate_below_the_source_converts() {
        let request = stream_request(None, Some(128), None);
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &aac_source(Some(320.0))),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &flac_source()),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
        assert_eq!(
            Representation::resolve(
                &request,
                &empty_rules(),
                &SourceAudio {
                    codec: None,
                    bitrate_kbps: None,
                    lossless: false,
                }
            ),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
    }

    #[test]
    fn a_max_bitrate_above_the_source_keeps_the_original() {
        let request = stream_request(None, Some(320), None);
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &aac_source(Some(192.0))),
            Representation::Original
        );
    }

    #[test]
    fn a_fractional_source_bitrate_above_the_limit_still_converts() {
        let source = SourceAudio {
            codec: Some("mp4a.40.2".to_owned()),
            bitrate_kbps: Some(128.4),
            lossless: false,
        };

        assert_eq!(
            Representation::resolve(
                &stream_request(None, Some(128), None),
                &empty_rules(),
                &source
            ),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
        assert_eq!(
            Representation::resolve(
                &stream_request(Some("aac"), Some(128), None),
                &empty_rules(),
                &source
            ),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
    }

    #[test]
    fn the_encode_bitrate_is_capped_and_floored() {
        let request = stream_request(None, Some(512), None);
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &flac_source()),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 256 })
        );

        let low = stream_request(None, Some(16), None);
        assert_eq!(
            Representation::resolve(&low, &empty_rules(), &flac_source()),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 32 })
        );
    }

    #[test]
    fn an_aac_source_below_the_max_bitrate_is_copied() {
        let request = stream_request(Some("aac"), Some(256), None);
        assert_eq!(
            Representation::resolve(&request, &empty_rules(), &aac_source(Some(128.0))),
            Representation::Aac(AacMode::Copy)
        );
    }

    #[test]
    fn the_client_rule_converts_only_after_the_request_does_not_decide() {
        let formats = rules("Arpeggi=aac,Feishin=raw");
        assert_eq!(
            Representation::resolve(
                &stream_request(None, None, Some("Arpeggi")),
                &formats,
                &flac_source()
            ),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 256 })
        );
        assert_eq!(
            Representation::resolve(
                &stream_request(None, None, Some("Feishin")),
                &formats,
                &flac_source()
            ),
            Representation::Original
        );
        assert_eq!(
            Representation::resolve(
                &stream_request(Some("raw"), None, Some("Arpeggi")),
                &formats,
                &flac_source()
            ),
            Representation::Original
        );
        assert_eq!(
            Representation::resolve(
                &stream_request(None, None, Some("Sonixd")),
                &formats,
                &flac_source()
            ),
            Representation::Original
        );
    }

    #[test]
    fn an_unknown_format_falls_through_the_rules() {
        let formats = rules("Arpeggi=aac");
        assert_eq!(
            Representation::resolve(
                &stream_request(Some("mp3"), None, None),
                &formats,
                &flac_source()
            ),
            Representation::Original
        );
        assert_eq!(
            Representation::resolve(
                &stream_request(Some("opus"), Some(64), None),
                &formats,
                &flac_source()
            ),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 64 })
        );
        assert_eq!(
            Representation::resolve(
                &stream_request(Some("mp3"), None, Some("Arpeggi")),
                &formats,
                &flac_source()
            ),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 256 })
        );
    }

    #[test]
    fn client_formats_parse_and_reject_invalid_entries() {
        let formats = rules("Arpeggi=aac, Feishin=raw");
        assert_eq!(formats.format_for("Arpeggi"), Some(ClientFormat::Aac));
        assert_eq!(formats.format_for("Feishin"), Some(ClientFormat::Raw));
        assert_eq!(formats.format_for("arpeggi"), None);
        assert!(formats.is_aac_client("Arpeggi"));
        assert!(!formats.is_aac_client("Feishin"));
        assert!(!formats.is_aac_client("arpeggi"));

        assert!(ClientFormats::parse("").unwrap().is_empty());
        for raw in ["Arpeggi", "Arpeggi=mp3", "=aac", "Arpeggi=aac=raw"] {
            assert!(ClientFormats::parse(raw).is_none(), "accepted {raw}");
        }
    }

    #[test]
    fn the_estimate_covers_the_encoded_duration_and_frames() {
        assert_eq!(estimate_content_length(1_000, 256), 32_955);
        assert_eq!(
            Representation::Aac(AacMode::Copy).estimate_bytes(1_000, Some(256.0)),
            estimate_content_length(1_000, 256)
        );
        assert_eq!(
            Representation::Aac(AacMode::Copy).estimate_bytes(1_000, None),
            estimate_content_length(1_000, 128)
        );
        assert_eq!(
            Representation::Aac(AacMode::Copy).estimate_bytes(1_000, Some(0.5)),
            379
        );
        assert_eq!(
            Representation::Original.estimate_bytes(1_000, Some(256.0)),
            0
        );
    }

    #[test]
    fn codec_classification_matches_aac_and_lossless_names() {
        for codec in ["aac", "AAC", "mp4a.40.2", "mp4a.40.5"] {
            assert!(is_aac_codec(codec), "{codec}");
        }
        for codec in ["opus", "flac", "mp3"] {
            assert!(!is_aac_codec(codec), "{codec}");
        }

        for codec in ["flac", "alac", "pcm_s16le", "wavpack"] {
            assert!(is_lossless_codec(codec), "{codec}");
        }
        for codec in ["aac", "opus", "mp3"] {
            assert!(!is_lossless_codec(codec), "{codec}");
        }

        assert!(is_lossless_content_type("audio/flac"));
        assert!(!is_lossless_content_type("audio/mpeg"));
    }
}
