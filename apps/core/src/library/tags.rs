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
    let Some(artist) = text(tag, |tag| tag.artist().map(|value| value.into_owned())) else {
        return Err(TagError::Untagged);
    };

    Ok(TrackTags {
        title,
        artists: split_artists(&artist),
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

fn split_artists(artist: &str) -> Vec<String> {
    artist
        .split([';', '/'])
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
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
            duration_ms: None,
        }
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
}
