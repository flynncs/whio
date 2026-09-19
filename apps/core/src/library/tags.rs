use std::path::Path;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::Tag;

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("audio file could not be read")]
    Unreadable,

    #[error("audio file carries no usable tags")]
    Untagged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRung {
    MusicBrainzRecording,
    Isrc,
    Path,
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
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub release_type: Option<String>,
    pub release_status: Option<String>,
    pub country: Option<String>,
    pub compilation: Option<bool>,
    pub acoustid_fingerprint: Option<String>,
    pub artist_ids: Vec<String>,
    pub duration_ms: Option<u64>,
}

pub fn read_tags(path: &Path) -> Result<TrackTags, TagError> {
    let tagged = Probe::open(path)
        .map_err(|_| TagError::Unreadable)?
        .options(ParseOptions::new().read_properties(true))
        .read()
        .map_err(|_| TagError::Unreadable)?;

    let duration_ms = tagged.properties().duration().as_millis() as u64;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let Some(tag) = tag else {
        return Err(TagError::Untagged);
    };
    let Some(title) = text(tag, |tag| tag.title().map(|value| value.into_owned())) else {
        return Err(TagError::Untagged);
    };
    let Some(artists) = artist_credits(tag) else {
        return Err(TagError::Untagged);
    };

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
        isrc: item(tag, &ItemKey::Isrc),
        track_number: tag.track(),
        track_total: tag.track_total(),
        disc_number: tag.disk(),
        disc_total: tag.disk_total(),
        year: tag.year().map(|year| year as i32),
        genre: text(tag, |tag| tag.genre().map(|value| value.into_owned())),
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
    })
}

pub fn identity_for(tags: &TrackTags, relative_path: &str) -> (IdentityRung, String) {
    if let Some(recording_id) = non_empty(tags.recording_id.as_deref()) {
        return (IdentityRung::MusicBrainzRecording, recording_id.to_owned());
    }
    if let Some(isrc) = non_empty(tags.isrc.as_deref()) {
        return (IdentityRung::Isrc, isrc.to_owned());
    }
    (IdentityRung::Path, relative_path.to_owned())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(recording_id: Option<&str>, isrc: Option<&str>) -> TrackTags {
        TrackTags {
            title: "Song".to_owned(),
            artists: vec!["Artist".to_owned()],
            album: None,
            album_artist: None,
            album_artist_id: None,
            release_group_id: None,
            release_id: None,
            recording_id: recording_id.map(str::to_owned),
            isrc: isrc.map(str::to_owned),
            track_number: None,
            track_total: None,
            disc_number: None,
            disc_total: None,
            year: None,
            genre: None,
            label: None,
            release_type: None,
            release_status: None,
            country: None,
            compilation: None,
            acoustid_fingerprint: None,
            artist_ids: Vec::new(),
            duration_ms: None,
        }
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
    fn identity_prefers_the_recording_id_then_isrc_then_the_path() {
        assert_eq!(
            identity_for(&tags(Some("recording-1"), Some("isrc-1")), "a/b.flac"),
            (IdentityRung::MusicBrainzRecording, "recording-1".to_owned())
        );
        assert_eq!(
            identity_for(&tags(None, Some("isrc-1")), "a/b.flac"),
            (IdentityRung::Isrc, "isrc-1".to_owned())
        );
        assert_eq!(
            identity_for(&tags(None, None), "a/b.flac"),
            (IdentityRung::Path, "a/b.flac".to_owned())
        );
    }

    #[test]
    fn identity_ignores_blank_values() {
        assert_eq!(
            identity_for(&tags(Some("   "), Some("isrc-1")), "a/b.flac"),
            (IdentityRung::Isrc, "isrc-1".to_owned()),
            "a blank recording id is not an identity"
        );
        assert_eq!(
            identity_for(&tags(None, Some("  ")), "a/b.flac"),
            (IdentityRung::Path, "a/b.flac".to_owned()),
            "a blank isrc is not an identity"
        );
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
}
