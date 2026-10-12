use super::ports::{ExpectedTrack, MbRecording, MbRelease};
use super::selection::{lengths_agree, normalise};

use crate::features::music_library::domain::{
    AlbumEntry, MatchRecord, normalize_for_matching, strict_recording_match,
};

pub(super) fn collapse_album_entries(entries: &[AlbumEntry]) -> Vec<&AlbumEntry> {
    let mut kept: Vec<&AlbumEntry> = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        let remote = album_record(entry);
        let candidates = entries
            .iter()
            .enumerate()
            .filter(|(candidate_position, candidate)| {
                *candidate_position != position && same_primary_artist(candidate, entry)
            })
            .map(|(_, candidate)| album_record(candidate))
            .collect();
        let unidentified = entry
            .track()
            .metadata()
            .musicbrainz_recording_id()
            .is_none()
            && entry.track().metadata().isrc().is_none();
        let duplicate = strict_recording_match(&remote, candidates).is_some_and(|target| {
            kept.iter()
                .any(|candidate| candidate.track().id().to_string() == target)
        });
        if !unidentified || !duplicate {
            kept.push(entry);
        }
    }
    kept
}

fn same_primary_artist(left: &AlbumEntry, right: &AlbumEntry) -> bool {
    match (
        left.track().artists().first(),
        right.track().artists().first(),
    ) {
        (Some(left), Some(right)) => {
            left.musicbrainz_artist_id() == right.musicbrainz_artist_id()
                && normalize_for_matching(left.name()) == normalize_for_matching(right.name())
        }
        _ => false,
    }
}

fn album_record(entry: &AlbumEntry) -> MatchRecord {
    let track = entry.track();
    MatchRecord::new(
        track.id().to_string(),
        track.title(),
        track
            .artists()
            .iter()
            .map(|artist| artist.name().to_owned())
            .collect(),
        track.duration_ms(),
        "",
        track
            .metadata()
            .musicbrainz_recording_id()
            .map(str::to_owned),
        track.metadata().isrc().map(str::to_owned),
        None,
        true,
    )
}

const PIN_LENGTH_TOLERANCE_MS: i64 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pinned<T> {
    Unique(T),
    NotFound,
    Ambiguous,
}

pub fn pin_release(candidates: Vec<MbRelease>, expected: &[ExpectedTrack]) -> Pinned<MbRelease> {
    let survivors = candidates
        .into_iter()
        .filter(|release| matches_release(release, expected))
        .collect::<Vec<_>>();

    if survivors.is_empty() {
        return Pinned::NotFound;
    }

    let mut signatures: Vec<Vec<(String, Option<u32>)>> = Vec::new();
    for release in &survivors {
        let signature = release
            .tracks
            .iter()
            .map(|track| (normalise(&track.title), track.length_ms))
            .collect::<Vec<_>>();

        if !signatures.contains(&signature) {
            signatures.push(signature);
        }
    }

    if signatures.len() > 1 {
        return Pinned::Ambiguous;
    }

    Pinned::Unique(survivors.into_iter().next().expect("a survivor exists"))
}

fn matches_release(release: &MbRelease, expected: &[ExpectedTrack]) -> bool {
    release.tracks.len() == expected.len()
        && lengths_agree(
            release
                .tracks
                .iter()
                .map(|track| track.length_ms.map(i64::from))
                .collect(),
            expected
                .iter()
                .map(|track| track.length_ms.map(i64::from))
                .collect(),
            PIN_LENGTH_TOLERANCE_MS,
        )
}

pub fn pin_recording(
    candidates: Vec<MbRecording>,
    title: &str,
    length_ms: Option<u32>,
) -> Pinned<MbRecording> {
    let wanted = normalise(title);
    let matches = candidates
        .into_iter()
        .filter(|recording| recording_matches(recording, &wanted, length_ms))
        .collect::<Vec<_>>();

    match matches.len() {
        0 => Pinned::NotFound,
        1 => Pinned::Unique(matches.into_iter().next().expect("one match")),
        _ => Pinned::Ambiguous,
    }
}

fn recording_matches(recording: &MbRecording, wanted: &str, length_ms: Option<u32>) -> bool {
    if normalise(&recording.title) != wanted {
        return false;
    }

    match (recording.length_ms, length_ms) {
        (Some(candidate), Some(wanted)) => {
            (i64::from(candidate) - i64::from(wanted)).abs() <= PIN_LENGTH_TOLERANCE_MS
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::acquisition::ports::MbId;
    use uuid::Uuid;

    fn release(number: u8, lengths: &[Option<u32>]) -> MbRelease {
        MbRelease {
            id: MbId::new(Uuid::from_u128(u128::from(number))),
            artist: "An Artist".to_owned(),
            title: "An Album".to_owned(),
            tracks: lengths
                .iter()
                .enumerate()
                .map(|(index, length)| ExpectedTrack {
                    disc: 1,
                    position: u16::try_from(index + 1).unwrap(),
                    title: format!("Track {}", index + 1),
                    length_ms: *length,
                })
                .collect(),
        }
    }

    fn expected(lengths: &[Option<u32>]) -> Vec<ExpectedTrack> {
        release(0, lengths).tracks
    }

    fn recording(number: u8, title: &str, length_ms: Option<u32>) -> MbRecording {
        MbRecording {
            id: MbId::new(Uuid::from_u128(u128::from(number))),
            artist: "An Artist".to_owned(),
            title: title.to_owned(),
            length_ms,
        }
    }

    fn album_entry(title: &str, duration: Option<u64>) -> AlbumEntry {
        use crate::features::music_library::domain::{AlbumTrack, Track, TrackId, TrackMetadata};
        AlbumEntry::new(
            Track::new(
                TrackId::new(),
                TrackMetadata::from_names(title.to_owned(), vec!["An Artist".to_owned()], duration)
                    .unwrap(),
            ),
            AlbumTrack::unplaced(),
        )
    }

    #[test]
    fn collapse_keeps_first_occurrences_in_album_order() {
        let entries = vec![
            album_entry("Spark", Some(200_000)),
            album_entry("Satellite", Some(180_000)),
            album_entry("Spark", Some(201_000)),
            album_entry("Satellite", Some(180_000)),
        ];
        let collapsed = collapse_album_entries(&entries);
        assert_eq!(collapsed.len(), 2);
        assert_eq!(collapsed[0].track().id(), entries[0].track().id());
        assert_eq!(collapsed[1].track().id(), entries[1].track().id());
    }

    #[test]
    fn collapse_preserves_outside_tolerance_missing_duration_and_versions() {
        let entries = vec![
            album_entry("Spark", Some(200_000)),
            album_entry("Spark", Some(202_001)),
            album_entry("Spark", None),
            album_entry("Spark (Live)", Some(200_000)),
        ];
        assert_eq!(collapse_album_entries(&entries).len(), 4);
    }

    #[test]
    fn collapse_keeps_same_named_identified_artists_separate() {
        use crate::features::music_library::domain::{
            AlbumTrack, ArtistCredit, Track, TrackId, TrackMetadata,
        };
        let entries = ["artist-one", "artist-two"]
            .into_iter()
            .map(|identifier| {
                AlbumEntry::new(
                    Track::new(
                        TrackId::new(),
                        TrackMetadata::new(
                            "Spark".to_owned(),
                            vec![
                                ArtistCredit::new(
                                    "An Artist".to_owned(),
                                    Some(identifier.to_owned()),
                                )
                                .unwrap(),
                            ],
                            Some(200_000),
                        )
                        .unwrap(),
                    ),
                    AlbumTrack::unplaced(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(collapse_album_entries(&entries).len(), 2);
    }

    #[test]
    fn collapse_preserves_full_album_ambiguity_for_feature_titles() {
        let entries = vec![
            album_entry("Song", Some(200_000)),
            album_entry("Song (feat. Guest)", Some(200_000)),
            album_entry("Song (feat. Guest)", Some(200_000)),
        ];
        assert_eq!(
            collapse_album_entries(&entries)
                .iter()
                .map(|entry| entry.track().id())
                .collect::<Vec<_>>(),
            entries
                .iter()
                .map(|entry| entry.track().id())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn collapse_keeps_three_indistinguishable_tracks_when_the_matcher_abstains() {
        let entries = vec![
            album_entry("Song", Some(200_000)),
            album_entry("Song", Some(200_000)),
            album_entry("Song", Some(200_000)),
        ];
        assert_eq!(collapse_album_entries(&entries).len(), 3);
    }

    #[test]
    fn collapse_does_not_choose_among_several_matches() {
        let entries = vec![
            album_entry("Spark", Some(200_000)),
            album_entry("Spark", Some(203_000)),
            album_entry("Spark", Some(201_500)),
        ];
        assert_eq!(collapse_album_entries(&entries).len(), 3);
    }

    #[test]
    fn a_single_matching_tracklist_pins_the_first_release() {
        let candidates = vec![release(1, &[Some(200_000), Some(180_000)])];

        match pin_release(candidates, &expected(&[Some(201_000), Some(181_000)])) {
            Pinned::Unique(release) => assert_eq!(release.id, MbId::new(Uuid::from_u128(1))),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn identical_tracklists_from_several_releases_pin_the_first() {
        let candidates = vec![release(1, &[Some(200_000)]), release(2, &[Some(200_000)])];

        match pin_release(candidates, &expected(&[Some(200_000)])) {
            Pinned::Unique(release) => assert_eq!(release.id, MbId::new(Uuid::from_u128(1))),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn different_matching_tracklists_are_ambiguous() {
        let mut first = release(1, &[Some(200_000)]);
        let mut second = release(2, &[Some(200_000)]);
        first.tracks[0].title = "One".to_owned();
        second.tracks[0].title = "Different".to_owned();

        assert_eq!(
            pin_release(vec![first, second], &expected(&[Some(200_000)])),
            Pinned::Ambiguous
        );
    }

    #[test]
    fn a_track_count_mismatch_is_not_found() {
        let candidates = vec![release(1, &[Some(200_000)])];

        assert_eq!(
            pin_release(candidates, &expected(&[Some(200_000), Some(180_000)])),
            Pinned::NotFound
        );
    }

    #[test]
    fn lengths_outside_three_seconds_do_not_match() {
        let candidates = vec![release(1, &[Some(200_000)])];

        assert_eq!(
            pin_release(candidates, &expected(&[Some(204_000)])),
            Pinned::NotFound
        );
    }

    #[test]
    fn unknown_lengths_skip_the_length_rule() {
        let candidates = vec![release(1, &[None])];

        assert!(matches!(
            pin_release(candidates, &expected(&[Some(200_000)])),
            Pinned::Unique(_)
        ));
    }

    #[test]
    fn a_recording_matches_on_normalised_title_and_length() {
        let candidates = vec![
            recording(1, "A Track (Live)", Some(200_000)),
            recording(2, "A Track", Some(210_000)),
        ];

        assert!(matches!(
            pin_recording(candidates.clone(), "a track", Some(211_000)),
            Pinned::Unique(recording) if recording.id == MbId::new(Uuid::from_u128(2))
        ));
        assert_eq!(
            pin_recording(candidates, "a track", Some(240_000)),
            Pinned::NotFound
        );
    }
}
