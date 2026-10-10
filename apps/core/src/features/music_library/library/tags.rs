use std::path::Path;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::properties::FileProperties;
use lofty::tag::Tag;

use crate::features::music_library::domain::{CreditRole, PartialDate, normalize_for_matching};

const MAX_GENRE: usize = 200;
const MAX_GENRES: usize = 16;
const MAX_CREDIT_NAME: usize = 500;

fn isrcs(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(|value| {
            value
                .split(|character: char| {
                    character == ';' || character == ',' || character.is_whitespace()
                })
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter_map(|value| isrc(&value))
        .collect()
}

fn isrc(value: &str) -> Option<String> {
    let normalized = value.trim().replace('-', "").to_ascii_uppercase();
    let bytes = normalized.as_bytes();
    let valid = bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..5]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes[5..].iter().all(u8::is_ascii_digit);
    valid.then_some(normalized)
}

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("audio file could not be read")]
    Unreadable,

    #[error("audio file carries no usable tags")]
    Untagged,

    #[error("audio file has no usable title")]
    MissingTitle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCredit {
    pub role: CreditRole,
    pub name: String,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTags {
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub album_artist_id: Option<String>,
    pub release_group_id: Option<String>,
    pub release_id: Option<String>,
    pub recording_id: Option<String>,
    pub isrc: Option<String>,
    pub invalid_isrc: bool,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub credits: Vec<TagCredit>,
    pub recording_date: Option<PartialDate>,
    pub original_date: Option<PartialDate>,
    pub release_date: Option<PartialDate>,
    pub label: Option<String>,
    pub release_type: Option<String>,
    pub release_status: Option<String>,
    pub country: Option<String>,
    pub compilation: Option<bool>,
    pub acoustid_fingerprint: Option<String>,
    pub artist_ids: Vec<String>,
    pub duration_ms: Option<u64>,
    pub container: Option<String>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
}

pub fn read_tags(path: &Path) -> Result<TrackTags, TagError> {
    let tagged = Probe::open(path)
        .map_err(|_| TagError::Unreadable)?
        .options(ParseOptions::new().read_properties(true))
        .guess_file_type()
        .map_err(|_| TagError::Unreadable)?
        .read()
        .map_err(|_| TagError::Unreadable)?;

    let duration_ms = tagged.properties().duration().as_millis() as u64;
    let representation = representation(tagged.file_type(), tagged.properties());
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let Some(tag) = tag else {
        return Err(TagError::Untagged);
    };
    let Some(title) = text(tag, |tag| tag.title().map(|value| value.into_owned())) else {
        return Err(TagError::MissingTitle);
    };
    let artists = artist_credits(tag).unwrap_or_default();

    let artist_ids = tag
        .get_strings(&ItemKey::MusicBrainzArtistId)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();

    let compilation = tag
        .get_string(&ItemKey::FlagCompilation)
        .and_then(parse_compilation)
        .or_else(|| raw_text(tag, &["COMPILATION"]).and_then(|value| parse_compilation(&value)));

    Ok(TrackTags {
        title,
        artists,
        album: text(tag, |tag| tag.album().map(|value| value.into_owned())),
        album_artist: item(tag, &ItemKey::AlbumArtist),
        album_artist_id: item(tag, &ItemKey::MusicBrainzReleaseArtistId),
        release_group_id: item(tag, &ItemKey::MusicBrainzReleaseGroupId),
        release_id: item(tag, &ItemKey::MusicBrainzReleaseId),
        recording_id: item(tag, &ItemKey::MusicBrainzRecordingId),
        isrc: isrcs(tag.get_strings(&ItemKey::Isrc).map(str::to_owned))
            .first()
            .cloned(),
        invalid_isrc: tag
            .get_strings(&ItemKey::Isrc)
            .any(|value| !value.trim().is_empty() && isrcs([value.to_owned()]).is_empty()),
        track_number: tag.track(),
        track_total: tag.track_total(),
        disc_number: tag.disk(),
        disc_total: tag.disk_total(),
        year: tag.year().map(|year| year as i32),
        genres: genres(tag),
        credits: role_credits(tag),
        recording_date: date(tag, &ItemKey::RecordingDate),
        original_date: date(tag, &ItemKey::OriginalReleaseDate),
        release_date: date(tag, &ItemKey::ReleaseDate),
        label: item(tag, &ItemKey::Label).or_else(|| raw_text(tag, &["LABEL", "ORGANIZATION"])),
        release_type: raw_text(
            tag,
            &["MUSICBRAINZ_ALBUMTYPE", "RELEASETYPE", "MB_ALBUMTYPE"],
        ),
        release_status: raw_text(
            tag,
            &["MUSICBRAINZ_ALBUMSTATUS", "RELEASESTATUS", "MB_ALBUMSTATUS"],
        ),
        country: raw_text(
            tag,
            &[
                "RELEASECOUNTRY",
                "MUSICBRAINZ_ALBUMCOUNTRY",
                "RELEASE_COUNTRY",
            ],
        ),
        compilation,
        acoustid_fingerprint: raw_text(tag, &["ACOUSTID_FINGERPRINT", "ACOUSTID FINGERPRINT"]),
        artist_ids,
        duration_ms: Some(duration_ms),
        container: representation.container,
        codec: representation.codec,
        bitrate_kbps: representation.bitrate_kbps,
        sample_rate_hz: representation.sample_rate_hz,
        bit_depth: representation.bit_depth,
        channels: representation.channels,
    })
}

struct Representation {
    container: Option<String>,
    codec: Option<String>,
    bitrate_kbps: Option<u32>,
    sample_rate_hz: Option<u32>,
    bit_depth: Option<u8>,
    channels: Option<u8>,
}

pub(crate) fn content_type_for_container(container: &str) -> Option<&'static str> {
    match container {
        "flac" => Some("audio/flac"),
        "mpeg" => Some("audio/mpeg"),
        "mp4" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "ogg" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "aiff" => Some("audio/aiff"),
        _ => None,
    }
}

fn representation(file_type: FileType, properties: &FileProperties) -> Representation {
    let (container, codec) = match file_type {
        FileType::Flac => (Some("flac".to_owned()), Some("flac".to_owned())),
        FileType::Mpeg => (Some("mpeg".to_owned()), None),
        FileType::Mp4 => (Some("mp4".to_owned()), None),
        FileType::Aac => (Some("aac".to_owned()), Some("aac".to_owned())),
        FileType::Opus => (Some("ogg".to_owned()), Some("opus".to_owned())),
        FileType::Vorbis => (Some("ogg".to_owned()), Some("vorbis".to_owned())),
        FileType::Speex => (Some("ogg".to_owned()), Some("speex".to_owned())),
        FileType::Wav => (Some("wav".to_owned()), None),
        FileType::Aiff => (Some("aiff".to_owned()), None),
        FileType::Ape => (Some("ape".to_owned()), None),
        FileType::Mpc => (Some("mpc".to_owned()), None),
        FileType::WavPack => (Some("wavpack".to_owned()), None),
        FileType::Custom(name) => (custom_container(name), None),
        _ => (None, None),
    };

    Representation {
        container,
        codec,
        bitrate_kbps: positive(properties.audio_bitrate())
            .or_else(|| positive(properties.overall_bitrate())),
        sample_rate_hz: positive(properties.sample_rate()),
        bit_depth: positive(properties.bit_depth()),
        channels: positive(properties.channels()),
    }
}

fn custom_container(name: &str) -> Option<String> {
    let plausible = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit());

    plausible.then(|| name.to_owned())
}

fn positive<T: Default + PartialOrd>(value: Option<T>) -> Option<T> {
    value.filter(|value| *value > T::default())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedPicture {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

pub const LOCAL_MAX_PICTURE_BYTES: u64 = 32 * 1024 * 1024;

pub fn read_cover_art(path: &Path) -> Result<Option<EmbeddedPicture>, TagError> {
    let tagged = Probe::open(path)
        .map_err(|_| TagError::Unreadable)?
        .options(ParseOptions::new().read_properties(false))
        .read()
        .map_err(|_| TagError::Unreadable)?;

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let Some(tag) = tag else {
        return Ok(None);
    };

    let mut fallback = None;
    for picture in tag.pictures() {
        let Some(embedded) = embedded_picture(picture) else {
            continue;
        };
        if picture.pic_type() == PictureType::CoverFront {
            return Ok(Some(embedded));
        }
        if fallback.is_none() {
            fallback = Some(embedded);
        }
    }

    Ok(fallback)
}

fn embedded_picture(picture: &Picture) -> Option<EmbeddedPicture> {
    if picture.data().len() as u64 > LOCAL_MAX_PICTURE_BYTES {
        return None;
    }
    let content_type = picture_content_type(picture)?;
    Some(EmbeddedPicture {
        content_type,
        bytes: picture.data().to_vec(),
    })
}

fn picture_content_type(picture: &Picture) -> Option<String> {
    if let Some(mime) = picture.mime_type()
        && !matches!(mime, MimeType::Unknown(_))
    {
        return Some(mime.as_str().to_owned());
    }

    sniff_picture(picture.data())
}

pub(crate) fn sniff_picture(data: &[u8]) -> Option<String> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg".to_owned())
    } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png".to_owned())
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif".to_owned())
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp".to_owned())
    } else {
        None
    }
}

#[cfg(test)]
pub(crate) fn write_pictured_flac(destination: &Path, pictures: Vec<Picture>) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
    std::fs::copy(fixture, destination).expect("the fixture copies");

    let mut tagged = Probe::open(destination)
        .expect("the fixture opens")
        .read()
        .expect("the fixture reads");
    {
        let has_primary = tagged.primary_tag_mut().is_some();
        let tag = if has_primary {
            tagged.primary_tag_mut().expect("checked above")
        } else {
            tagged.first_tag_mut().expect("the fixture carries a tag")
        };
        for picture in pictures {
            tag.push_picture(picture);
        }
    }
    tagged
        .save_to_path(destination, lofty::config::WriteOptions::default())
        .expect("the pictured file saves");
}

#[cfg(test)]
pub(crate) fn tagged_flac(path: &Path, items: &[(ItemKey, &str)]) {
    use lofty::{
        config::WriteOptions,
        file::{AudioFile, TaggedFileExt},
        tag::{ItemValue, TagItem},
    };

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
    std::fs::copy(fixture, path).expect("the fixture copies");

    let mut tagged = Probe::open(path)
        .expect("the fixture opens")
        .read()
        .expect("the fixture reads");
    {
        let has_primary = tagged.primary_tag_mut().is_some();
        let tag = if has_primary {
            tagged.primary_tag_mut().expect("checked above")
        } else {
            tagged.first_tag_mut().expect("the fixture carries a tag")
        };

        for key in [
            ItemKey::Genre,
            ItemKey::TrackArtist,
            ItemKey::TrackArtists,
            ItemKey::AlbumTitle,
            ItemKey::Composer,
            ItemKey::Performer,
            ItemKey::MixDj,
            ItemKey::Arranger,
            ItemKey::Conductor,
            ItemKey::Director,
            ItemKey::Engineer,
            ItemKey::Lyricist,
            ItemKey::MixEngineer,
            ItemKey::Producer,
            ItemKey::Remixer,
            ItemKey::RecordingDate,
            ItemKey::OriginalReleaseDate,
            ItemKey::ReleaseDate,
            ItemKey::TrackTotal,
            ItemKey::DiscTotal,
            ItemKey::MusicBrainzRecordingId,
            ItemKey::FlagCompilation,
            ItemKey::Unknown("COMPILATION".to_owned()),
            ItemKey::Unknown("RELEASECOUNTRY".to_owned()),
            ItemKey::Unknown("DJMIXER".to_owned()),
            ItemKey::Unknown("OTHERROLE".to_owned()),
        ] {
            tag.take(&key).count();
        }

        for (key, value) in items {
            let item = TagItem::new((*key).clone(), ItemValue::Text((*value).to_owned()));

            if matches!(item.key(), ItemKey::Unknown(_)) {
                tag.push_unchecked(item);
            } else {
                tag.push(item);
            }
        }
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .expect("the tagged file saves");
}

fn item(tag: &Tag, key: &ItemKey) -> Option<String> {
    tag.get_string(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn text(tag: &Tag, read: impl Fn(&Tag) -> Option<String>) -> Option<String> {
    read(tag)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn artist_credits(tag: &Tag) -> Option<Vec<String>> {
    let mut names: Vec<String> = tag
        .get_strings(&ItemKey::TrackArtist)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();

    if names.is_empty() {
        names = tag
            .get_strings(&ItemKey::TrackArtists)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
    }

    match names.len() {
        0 => None,
        1 => Some(split_credits(&names[0])),
        _ => Some(names),
    }
}

fn split_credits(artist: &str) -> Vec<String> {
    artist
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn raw_text(tag: &Tag, names: &[&str]) -> Option<String> {
    tag.items().find_map(|item| {
        let ItemKey::Unknown(key) = item.key() else {
            return None;
        };

        if !names.iter().any(|name| key.eq_ignore_ascii_case(name)) {
            return None;
        }

        item.value()
            .text()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parse_compilation(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn bounded(value: &str, max: usize) -> Option<String> {
    let value = value.trim();

    if value.is_empty() || value.chars().count() > max {
        return None;
    }

    Some(value.to_owned())
}

fn genres(tag: &Tag) -> Vec<String> {
    let mut accepted: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let values = tag
        .get_strings(&ItemKey::Genre)
        .map(str::to_owned)
        .chain(tag.genre().map(|genre| genre.into_owned()));

    for value in values {
        let Some(name) = bounded(&value, MAX_GENRE) else {
            continue;
        };
        let key = normalize_for_matching(&name);

        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        if accepted.len() == MAX_GENRES {
            break;
        }

        seen.push(key);
        accepted.push(name);
    }

    accepted
}

fn role_credits(tag: &Tag) -> Vec<TagCredit> {
    let mut credits = Vec::new();

    for (role, key) in [
        (CreditRole::Arranger, ItemKey::Arranger),
        (CreditRole::Composer, ItemKey::Composer),
        (CreditRole::Conductor, ItemKey::Conductor),
        (CreditRole::Director, ItemKey::Director),
        (CreditRole::Engineer, ItemKey::Engineer),
        (CreditRole::Lyricist, ItemKey::Lyricist),
        (CreditRole::Mixer, ItemKey::MixEngineer),
        (CreditRole::Performer, ItemKey::Performer),
        (CreditRole::Producer, ItemKey::Producer),
        (CreditRole::Remixer, ItemKey::Remixer),
    ] {
        for value in tag.get_strings(&key) {
            push_role_credit(&mut credits, role, value);
        }
    }

    if let Some(name) = item(tag, &ItemKey::MixDj).or_else(|| raw_text(tag, &["DJMIXER"])) {
        push_role_credit(&mut credits, CreditRole::Djmixer, &name);
    }

    credits
}

fn push_role_credit(credits: &mut Vec<TagCredit>, role: CreditRole, value: &str) {
    let name = match role {
        CreditRole::Performer => performer_name(value),
        _ => bounded(value, MAX_CREDIT_NAME),
    };

    if let Some(name) = name {
        credits.push(TagCredit {
            role,
            name,
            musicbrainz_artist_id: None,
        });
    }
}

fn performer_name(value: &str) -> Option<String> {
    let reduced = value.split(" (").next().unwrap_or(value);

    bounded(reduced, MAX_CREDIT_NAME)
}

fn first_value(tag: &Tag, key: &ItemKey) -> Option<String> {
    tag.get_strings(key)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn date(tag: &Tag, key: &ItemKey) -> Option<PartialDate> {
    first_value(tag, key).and_then(|value| PartialDate::parse(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::music_library::domain::DatePrecision;

    fn file_properties(
        overall_bitrate: Option<u32>,
        audio_bitrate: Option<u32>,
        sample_rate: Option<u32>,
    ) -> FileProperties {
        FileProperties::new(
            std::time::Duration::from_secs(1),
            overall_bitrate,
            audio_bitrate,
            sample_rate,
            Some(16),
            Some(2),
            None,
        )
    }

    #[test]
    fn the_container_table_states_only_what_the_file_implies() {
        let properties = file_properties(Some(900), Some(900), Some(44_100));

        for (file_type, container, codec, content_type) in [
            (
                FileType::Flac,
                Some("flac"),
                Some("flac"),
                Some("audio/flac"),
            ),
            (FileType::Mpeg, Some("mpeg"), None, Some("audio/mpeg")),
            (FileType::Mp4, Some("mp4"), None, Some("audio/mp4")),
            (FileType::Aac, Some("aac"), Some("aac"), Some("audio/aac")),
            (FileType::Opus, Some("ogg"), Some("opus"), Some("audio/ogg")),
            (
                FileType::Vorbis,
                Some("ogg"),
                Some("vorbis"),
                Some("audio/ogg"),
            ),
            (
                FileType::Speex,
                Some("ogg"),
                Some("speex"),
                Some("audio/ogg"),
            ),
            (FileType::Wav, Some("wav"), None, Some("audio/wav")),
            (FileType::Aiff, Some("aiff"), None, Some("audio/aiff")),
            (FileType::Ape, Some("ape"), None, None),
            (FileType::Mpc, Some("mpc"), None, None),
            (FileType::WavPack, Some("wavpack"), None, None),
            (FileType::Custom("myformat"), Some("myformat"), None, None),
            (FileType::Custom("not a format"), None, None, None),
        ] {
            let reported = representation(file_type, &properties);

            assert_eq!(reported.container.as_deref(), container, "{file_type:?}");
            assert_eq!(reported.codec.as_deref(), codec, "{file_type:?}");
            assert_eq!(
                reported
                    .container
                    .as_deref()
                    .and_then(content_type_for_container),
                content_type,
                "{file_type:?}"
            );
        }
    }

    #[test]
    fn a_non_positive_reported_value_is_absence() {
        let reported = representation(
            FileType::Wav,
            &FileProperties::new(
                std::time::Duration::from_secs(1),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                None,
            ),
        );

        assert_eq!(reported.bitrate_kbps, None);
        assert_eq!(reported.sample_rate_hz, None);
        assert_eq!(reported.bit_depth, None);
        assert_eq!(reported.channels, None);
        assert_eq!(reported.container.as_deref(), Some("wav"));
    }

    #[test]
    fn an_unreadable_audio_bitrate_falls_back_to_the_overall_bitrate() {
        let reported = representation(
            FileType::Mpeg,
            &file_properties(Some(320), Some(0), Some(44_100)),
        );

        assert_eq!(reported.bitrate_kbps, Some(320));
        assert_eq!(reported.sample_rate_hz, Some(44_100));
        assert_eq!(reported.bit_depth, Some(16));
        assert_eq!(reported.channels, Some(2));
    }

    #[test]
    fn the_wave_fixture_reports_its_container_and_its_real_properties() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.wav");
        let tags = read_tags(&fixture).unwrap();

        assert_eq!(tags.title, "Fixture Wave");
        assert_eq!(tags.container.as_deref(), Some("wav"));
        assert_eq!(tags.codec, None);
        assert_eq!(tags.bitrate_kbps, Some(706));
        assert_eq!(tags.sample_rate_hz, Some(44_100));
        assert_eq!(tags.bit_depth, Some(16));
        assert_eq!(tags.channels, Some(1));
    }

    #[test]
    fn compilation_flags_parse_the_common_spellings() {
        for value in ["1", "true", "TRUE", " yes "] {
            assert_eq!(parse_compilation(value), Some(true), "{value}");
        }
        for value in ["0", "false", "No"] {
            assert_eq!(parse_compilation(value), Some(false), "{value}");
        }
        assert_eq!(parse_compilation("maybe"), None);
    }

    #[test]
    fn read_tags_reads_the_album_and_source_identifiers() {
        use lofty::{
            config::WriteOptions,
            file::{AudioFile, TaggedFileExt},
            tag::{ItemValue, TagItem},
        };

        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
        let path = std::env::temp_dir().join(format!("whio-tags-{}.flac", uuid::Uuid::now_v7()));
        std::fs::copy(&fixture, &path).unwrap();

        let mut tagged = Probe::open(&path).unwrap().read().unwrap();
        {
            let has_primary = tagged.primary_tag_mut().is_some();
            let tag = if has_primary {
                tagged.primary_tag_mut().expect("checked above")
            } else {
                tagged.first_tag_mut().expect("the fixture carries a tag")
            };

            tag.insert_text(ItemKey::Label, "Test Label".to_owned());
            for (key, value) in [
                ("MUSICBRAINZ_ALBUMTYPE", "album"),
                ("MUSICBRAINZ_ALBUMSTATUS", "official"),
                ("RELEASECOUNTRY", "GB"),
                ("ACOUSTID_FINGERPRINT", "AQAA-test"),
            ] {
                tag.push_unchecked(TagItem::new(
                    ItemKey::Unknown(key.to_owned()),
                    ItemValue::Text(value.to_owned()),
                ));
            }
            tag.insert_text(ItemKey::FlagCompilation, "0".to_owned());

            tag.take(&ItemKey::MusicBrainzArtistId).count();
            tag.push(TagItem::new(
                ItemKey::MusicBrainzArtistId,
                ItemValue::Text("artist-one".to_owned()),
            ));
            tag.push(TagItem::new(
                ItemKey::MusicBrainzArtistId,
                ItemValue::Text("artist-two".to_owned()),
            ));
        }
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();

        let tags = read_tags(&path).unwrap();

        assert_eq!(tags.label.as_deref(), Some("Test Label"));
        assert_eq!(tags.release_type.as_deref(), Some("album"));
        assert_eq!(tags.release_status.as_deref(), Some("official"));
        assert_eq!(tags.country.as_deref(), Some("GB"));
        assert_eq!(tags.compilation, Some(false));
        assert_eq!(tags.acoustid_fingerprint.as_deref(), Some("AQAA-test"));
        assert_eq!(tags.artist_ids, vec!["artist-one", "artist-two"]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn credit_splitting_keeps_slashes_and_splits_semicolons() {
        assert_eq!(split_credits("AC/DC"), vec!["AC/DC"]);
        assert_eq!(
            split_credits("Earth, Wind & Fire"),
            vec!["Earth, Wind & Fire"]
        );
        assert_eq!(split_credits(" A ; B ; "), vec!["A", "B"]);
        assert_eq!(split_credits("  "), Vec::<String>::new());
    }

    #[test]
    fn multiple_artist_values_are_kept_in_order_without_splitting() {
        use lofty::{
            config::WriteOptions,
            file::{AudioFile, TaggedFileExt},
            tag::{ItemValue, TagItem},
        };

        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
        let path = std::env::temp_dir().join(format!("whio-artists-{}.flac", uuid::Uuid::now_v7()));
        std::fs::copy(&fixture, &path).unwrap();

        let mut tagged = Probe::open(&path).unwrap().read().unwrap();
        {
            let has_primary = tagged.primary_tag_mut().is_some();
            let tag = if has_primary {
                tagged.primary_tag_mut().expect("checked above")
            } else {
                tagged.first_tag_mut().expect("the fixture carries a tag")
            };

            tag.take(&ItemKey::TrackArtist).count();
            tag.push(TagItem::new(
                ItemKey::TrackArtist,
                ItemValue::Text("AC/DC".to_owned()),
            ));
            tag.push(TagItem::new(
                ItemKey::TrackArtist,
                ItemValue::Text("Daft Punk".to_owned()),
            ));
        }
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.artists, vec!["AC/DC", "Daft Punk"]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_with_no_artist_tag_still_reads() {
        use lofty::{
            config::WriteOptions,
            file::{AudioFile, TaggedFileExt},
        };

        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
        let path =
            std::env::temp_dir().join(format!("whio-noartist-{}.flac", uuid::Uuid::now_v7()));
        std::fs::copy(&fixture, &path).unwrap();

        let mut tagged = Probe::open(&path).unwrap().read().unwrap();
        {
            let has_primary = tagged.primary_tag_mut().is_some();
            let tag = if has_primary {
                tagged.primary_tag_mut().expect("checked above")
            } else {
                tagged.first_tag_mut().expect("the fixture carries a tag")
            };

            tag.take(&ItemKey::TrackArtist).count();
            tag.take(&ItemKey::TrackArtists).count();
        }
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();

        let tags = read_tags(&path).unwrap();
        assert!(tags.artists.is_empty());
        assert_eq!(tags.title, "Fixture Song");

        std::fs::remove_file(&path).unwrap();
    }

    fn temp_flac(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("whio-{label}-{}.flac", uuid::Uuid::now_v7()))
    }

    #[test]
    fn genres_are_multi_valued_deduplicated_on_the_normalised_value() {
        let path = temp_flac("genres");
        tagged_flac(
            &path,
            &[
                (ItemKey::Genre, "Rock"),
                (ItemKey::Genre, "rock "),
                (ItemKey::Genre, "ROCK"),
                (ItemKey::Genre, "Pop"),
            ],
        );

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.genres, vec!["Rock", "Pop"]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_genre_list_beyond_the_bound_keeps_the_first_sixteen_in_tag_order() {
        let names = (0..18)
            .map(|index| format!("genre-{index}"))
            .collect::<Vec<_>>();
        let items = names
            .iter()
            .map(|name| (ItemKey::Genre, name.as_str()))
            .collect::<Vec<_>>();
        let path = temp_flac("genre-bound");
        tagged_flac(&path, &items);

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.genres.len(), 16);
        assert_eq!(tags.genres[0], "genre-0");
        assert_eq!(tags.genres[15], "genre-15");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_over_long_or_blank_genre_is_dropped_rather_than_read() {
        let over_long = "g".repeat(201);
        let path = temp_flac("genre-invalid");
        tagged_flac(
            &path,
            &[
                (ItemKey::Genre, over_long.as_str()),
                (ItemKey::Genre, "   "),
                (ItemKey::Genre, "Jazz"),
            ],
        );

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.genres, vec!["Jazz"]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn role_credits_come_from_the_allowlisted_keys_only() {
        let path = temp_flac("roles");
        tagged_flac(
            &path,
            &[
                (ItemKey::Composer, "Clara Schumann"),
                (ItemKey::Performer, "Bill Evans (piano)"),
                (ItemKey::Performer, "Miles Davis (trumpet)"),
                (ItemKey::MixDj, "Ron Hardy"),
                (ItemKey::Unknown("OTHERROLE".to_owned()), "Nobody"),
            ],
        );

        let tags = read_tags(&path).unwrap();
        let credits = tags
            .credits
            .iter()
            .map(|credit| (credit.role, credit.name.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            credits,
            vec![
                (CreditRole::Composer, "Clara Schumann"),
                (CreditRole::Performer, "Bill Evans"),
                (CreditRole::Performer, "Miles Davis"),
                (CreditRole::Djmixer, "Ron Hardy"),
            ]
        );
        assert!(
            tags.credits
                .iter()
                .all(|credit| credit.musicbrainz_artist_id.is_none())
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_empty_performer_name_is_dropped() {
        let path = temp_flac("performer-empty");
        tagged_flac(
            &path,
            &[
                (ItemKey::Performer, " (piano)"),
                (ItemKey::Performer, "Alice Coltrane (harp)"),
            ],
        );

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.credits.len(), 1);
        assert_eq!(tags.credits[0].name, "Alice Coltrane");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_dates_keep_the_precision_the_tag_text_stated() {
        let path = temp_flac("dates");
        tagged_flac(
            &path,
            &[
                (ItemKey::RecordingDate, "1987"),
                (ItemKey::OriginalReleaseDate, "1979-05"),
                (ItemKey::ReleaseDate, "2001-03-12"),
            ],
        );

        let tags = read_tags(&path).unwrap();

        let recording = tags.recording_date.expect("the recording date parses");
        assert_eq!(recording.as_str(), "1987");
        assert_eq!(recording.precision(), DatePrecision::Year);
        assert_eq!(
            tags.original_date.as_ref().map(|date| date.as_str()),
            Some("1979-05".to_owned())
        );
        assert_eq!(
            tags.release_date.as_ref().map(|date| date.as_str()),
            Some("2001-03-12".to_owned())
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_unparsable_date_is_absence() {
        let path = temp_flac("date-invalid");
        tagged_flac(&path, &[(ItemKey::RecordingDate, "sometime")]);

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.recording_date, None);

        std::fs::remove_file(&path).unwrap();
    }

    fn cover(data: Vec<u8>, mime: Option<MimeType>) -> Picture {
        Picture::new_unchecked(PictureType::CoverFront, mime, None, data)
    }

    #[test]
    fn isrc_values_are_split_normalized_and_validated() {
        assert_eq!(
            isrcs(["GBGLM0700129;USJI10900291;USK6G0700001".to_owned()]),
            vec!["GBGLM0700129", "USJI10900291", "USK6G0700001"]
        );
        assert_eq!(
            isrcs(["bad", "USJI10900291"].map(str::to_owned)),
            vec!["USJI10900291"]
        );
        assert!(isrcs(["not-an-isrc".to_owned()]).is_empty());
        assert_eq!(isrcs(["gb-glm-07-00129".to_owned()]), vec!["GBGLM0700129"]);
    }

    #[test]
    fn read_tags_keeps_the_first_valid_isrc_across_tag_values() {
        let path = temp_flac("isrcs");
        tagged_flac(
            &path,
            &[
                (ItemKey::Isrc, "GBGLM0700129;USJI10900291;USK6G0700001"),
                (ItemKey::Isrc, "USAAA2400001"),
            ],
        );

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.isrc.as_deref(), Some("GBGLM0700129"));
        assert!(!tags.invalid_isrc);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_without_pictures_has_no_cover_art() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");

        assert_eq!(read_cover_art(&fixture).unwrap(), None);
    }

    #[test]
    fn an_embedded_front_cover_reads_back_with_its_mime_type() {
        let path = temp_flac("cover");
        let bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        write_pictured_flac(&path, vec![cover(bytes.clone(), Some(MimeType::Jpeg))]);

        let picture = read_cover_art(&path).unwrap().expect("cover art");
        assert_eq!(picture.content_type, "image/jpeg");
        assert_eq!(picture.bytes, bytes);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_front_cover_wins_over_an_earlier_picture() {
        let path = temp_flac("front");
        write_pictured_flac(
            &path,
            vec![
                Picture::new_unchecked(
                    PictureType::Other,
                    Some(MimeType::Png),
                    None,
                    vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
                ),
                cover(vec![0xFF, 0xD8, 0xFF], Some(MimeType::Jpeg)),
            ],
        );

        let picture = read_cover_art(&path).unwrap().expect("cover art");
        assert_eq!(picture.content_type, "image/jpeg");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_picture_without_a_declared_mime_type_is_sniffed() {
        let path = temp_flac("sniff");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0x00; 8]);
        write_pictured_flac(&path, vec![cover(png.clone(), None)]);

        let picture = read_cover_art(&path).unwrap().expect("cover art");
        assert_eq!(picture.content_type, "image/png");
        assert_eq!(picture.bytes, png);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_oversize_picture_is_skipped() {
        let picture = cover(
            vec![0xFF; LOCAL_MAX_PICTURE_BYTES as usize + 1],
            Some(MimeType::Jpeg),
        );

        assert_eq!(embedded_picture(&picture), None);
    }
}
