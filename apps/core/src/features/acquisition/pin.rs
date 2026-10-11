use super::ports::{ExpectedTrack, MbRecording, MbRelease};
use super::selection::{lengths_agree, normalise};

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
