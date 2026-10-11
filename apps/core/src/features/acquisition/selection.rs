use std::cmp::Reverse;

use unicode_normalization::UnicodeNormalization;

use super::ports::{CandidateFile, ReleaseCandidate, SearchText};

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "aac", "ogg", "opus", "wav", "aiff", "alac",
];
const LOSSLESS_EXTENSIONS: &[&str] = &["flac", "wav", "aiff", "alac"];
const EXCLUDED_VERSIONS: &[&str] = &[
    "live",
    "remix",
    "instrumental",
    "karaoke",
    "cover",
    "acoustic",
];
const FOLDER_LENGTH_TOLERANCE_MS: i64 = 5_000;
const RECORDING_LENGTH_TOLERANCE_MS: i64 = 3_000;

pub fn query_texts(artist: &str, title: &str) -> Vec<SearchText> {
    let full = format!("{artist} {title}");
    let stripped = clean(&full, true);
    let mut texts = Vec::new();

    if let Some(text) = SearchText::new(&stripped) {
        texts.push(text);
    }

    let kept = clean(&full, false);
    if kept != stripped
        && let Some(text) = SearchText::new(&kept)
    {
        texts.push(text);
    }

    texts
}

pub fn normalise(text: &str) -> String {
    clean(text, true)
}

fn clean(text: &str, strip: bool) -> String {
    let text = if strip {
        strip_bracketed(text)
    } else {
        text.to_owned()
    };
    let folded = text.nfkc().collect::<String>().to_lowercase();
    let filtered = folded
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect::<String>();

    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_bracketed(text: &str) -> String {
    let mut output = String::new();
    let mut depth = 0u32;

    for character in text.chars() {
        match character {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            other if depth == 0 => output.push(other),
            _ => {}
        }
    }

    output
}

fn extension(name: &str) -> Option<String> {
    let (_, extension) = name.rsplit_once('.')?;

    (!extension.is_empty()).then(|| extension.to_ascii_lowercase())
}

fn is_audio(file: &CandidateFile) -> bool {
    extension(&file.name).is_some_and(|extension| audio_name(&extension))
}

pub fn audio_name(extension: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
}

fn is_lossless(file: &CandidateFile) -> bool {
    extension(&file.name).is_some_and(|extension| LOSSLESS_EXTENSIONS.contains(&extension.as_str()))
}

fn studio_lossless(candidate: &ReleaseCandidate) -> bool {
    let audio = candidate.files.iter().filter(|file| is_audio(file)).count();

    audio > 0 && candidate.files.iter().all(is_lossless)
}

fn audio_files(candidate: &ReleaseCandidate) -> Vec<CandidateFile> {
    candidate
        .files
        .iter()
        .filter(|file| is_audio(file))
        .cloned()
        .collect()
}

pub fn accepted_for_release(
    candidates: Vec<ReleaseCandidate>,
    expected_count: usize,
    expected_lengths: &[Option<u32>],
    budget: usize,
) -> Vec<ReleaseCandidate> {
    let accepted = candidates
        .into_iter()
        .filter_map(|candidate| accept_release(candidate, expected_count, expected_lengths))
        .collect::<Vec<_>>();

    truncate(rank(accepted), budget)
}

fn accept_release(
    candidate: ReleaseCandidate,
    expected_count: usize,
    expected_lengths: &[Option<u32>],
) -> Option<ReleaseCandidate> {
    let audio = audio_files(&candidate);

    if audio.is_empty() || audio.len() != expected_count {
        return None;
    }
    if !lengths_agree(
        audio
            .iter()
            .map(|file| {
                file.facts
                    .duration_seconds
                    .map(|seconds| seconds as i64 * 1_000)
            })
            .collect(),
        expected_lengths
            .iter()
            .map(|length| length.map(i64::from))
            .collect(),
        FOLDER_LENGTH_TOLERANCE_MS,
    ) {
        return None;
    }

    Some(ReleaseCandidate {
        files: audio,
        ..candidate
    })
}

pub fn accepted_for_recording(
    candidates: Vec<ReleaseCandidate>,
    title: &str,
    length_ms: Option<u32>,
    budget: usize,
) -> Vec<ReleaseCandidate> {
    let accepted = candidates
        .into_iter()
        .flat_map(|candidate| accept_recordings(candidate, title, length_ms))
        .collect::<Vec<_>>();

    truncate(rank(accepted), budget)
}

fn accept_recordings(
    candidate: ReleaseCandidate,
    title: &str,
    length_ms: Option<u32>,
) -> Vec<ReleaseCandidate> {
    let wanted = normalise(title);
    let requested_has_excluded = has_excluded_version(&clean(title, false));

    candidate
        .files
        .iter()
        .filter(|file| is_audio(file))
        .filter(|file| {
            let stem = file
                .name
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(&file.name);

            if !clean(stem, true).contains(&wanted) {
                return false;
            }
            if !requested_has_excluded && has_excluded_version(&clean(stem, false)) {
                return false;
            }

            match (file.facts.duration_seconds, length_ms) {
                (Some(seconds), Some(length)) => {
                    (seconds as i64 * 1_000 - i64::from(length)).abs()
                        <= RECORDING_LENGTH_TOLERANCE_MS
                }
                _ => true,
            }
        })
        .map(|file| ReleaseCandidate {
            source: candidate.source.clone(),
            peer: candidate.peer.clone(),
            files: vec![file.clone()],
        })
        .collect()
}

fn has_excluded_version(stem: &str) -> bool {
    stem.split_whitespace()
        .any(|word| EXCLUDED_VERSIONS.contains(&word))
}

pub(crate) fn lengths_agree(
    actual: Vec<Option<i64>>,
    expected: Vec<Option<i64>>,
    tolerance: i64,
) -> bool {
    if actual.iter().any(Option::is_none) || expected.iter().any(Option::is_none) {
        return true;
    }

    let mut actual = actual.into_iter().flatten().collect::<Vec<_>>();
    let mut expected = expected.into_iter().flatten().collect::<Vec<_>>();
    actual.sort_unstable();
    expected.sort_unstable();

    actual
        .iter()
        .zip(expected.iter())
        .all(|(actual, expected)| (actual - expected).abs() <= tolerance)
}

pub fn rank(mut candidates: Vec<ReleaseCandidate>) -> Vec<ReleaseCandidate> {
    candidates.sort_by(|left, right| {
        lossless_key(left)
            .cmp(&lossless_key(right))
            .then(free_slot_key(left).cmp(&free_slot_key(right)))
            .then(queue_key(left).cmp(&queue_key(right)))
            .then(speed_key(left).cmp(&speed_key(right)))
    });

    candidates
}

fn lossless_key(candidate: &ReleaseCandidate) -> u8 {
    u8::from(!studio_lossless(candidate))
}

fn free_slot_key(candidate: &ReleaseCandidate) -> u8 {
    u8::from(candidate.peer.has_free_slot != Some(true))
}

fn queue_key(candidate: &ReleaseCandidate) -> (u8, u32) {
    match candidate.peer.queue_length {
        Some(length) => (0, length),
        None => (1, 0),
    }
}

fn speed_key(candidate: &ReleaseCandidate) -> (u8, Reverse<u64>) {
    match candidate.peer.upload_speed {
        Some(speed) => (0, Reverse(speed)),
        None => (1, Reverse(0)),
    }
}

fn truncate(mut candidates: Vec<ReleaseCandidate>, budget: usize) -> Vec<ReleaseCandidate> {
    candidates.truncate(budget);

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::acquisition::ports::{FileFacts, PeerFacts, TransferInput};

    fn file(name: &str, seconds: Option<u32>) -> CandidateFile {
        CandidateFile {
            input: TransferInput::SoulseekFile {
                peer: "peer-a".to_owned(),
                path: name.to_owned(),
                size_bytes: 1,
            },
            name: name.to_owned(),
            facts: FileFacts {
                duration_seconds: seconds,
                ..FileFacts::default()
            },
        }
    }

    fn candidate(source: &str, files: Vec<CandidateFile>) -> ReleaseCandidate {
        ReleaseCandidate {
            source: source.to_owned(),
            peer: PeerFacts::default(),
            files,
        }
    }

    fn texts(artist: &str, title: &str) -> Vec<String> {
        query_texts(artist, title)
            .into_iter()
            .map(|text| text.as_str().to_owned())
            .collect()
    }

    #[test]
    fn the_query_is_normalised() {
        assert_eq!(
            texts("Beyoncé [Deluxe]", "Crazy in Love (Remix)"),
            vec![
                "beyoncé crazy in love".to_owned(),
                "beyoncé deluxe crazy in love remix".to_owned()
            ]
        );
    }

    #[test]
    fn the_bracketed_variant_runs_only_when_different() {
        assert_eq!(texts("Artist", "Title"), vec!["artist title".to_owned()]);
        assert_eq!(texts("[x]", "y").len(), 2);
    }

    #[test]
    fn a_folder_with_the_wrong_audio_count_is_rejected() {
        let candidates = vec![candidate("a", vec![file("One.flac", None)])];

        assert!(accepted_for_release(candidates, 2, &[None, None], 10).is_empty());
    }

    #[test]
    fn zero_expected_tracks_never_accept_an_empty_audio_bundle() {
        let candidates = vec![
            candidate("non-audio", vec![file("cover.jpg", None)]),
            candidate("empty", Vec::new()),
            candidate("audio", vec![file("One.flac", None)]),
        ];

        assert!(accepted_for_release(candidates, 0, &[], 10).is_empty());
    }

    #[test]
    fn non_audio_files_are_ignored_for_the_count() {
        let candidates = vec![candidate(
            "a",
            vec![
                file("One.flac", None),
                file("Two.mp3", None),
                file("cover.jpg", None),
            ],
        )];
        let accepted = accepted_for_release(candidates, 2, &[None, None], 10);

        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].files.len(), 2);
    }

    #[test]
    fn folder_lengths_outside_five_seconds_are_rejected() {
        let expected = [Some(200_000), Some(180_000)];
        let far = vec![candidate(
            "a",
            vec![file("One.flac", Some(200)), file("Two.flac", Some(190))],
        )];
        assert!(accepted_for_release(far, 2, &expected, 10).is_empty());

        let close = vec![candidate(
            "a",
            vec![file("One.flac", Some(200)), file("Two.flac", Some(184))],
        )];
        assert_eq!(accepted_for_release(close, 2, &expected, 10).len(), 1);
    }

    #[test]
    fn excluded_versions_are_rejected_unless_requested() {
        let live = candidate("a", vec![file("Title (Live).flac", Some(200))]);
        assert!(accepted_for_recording(vec![live.clone()], "Title", Some(200_000), 10).is_empty());
        assert_eq!(
            accepted_for_recording(vec![live], "Title (Live)", Some(200_000), 10).len(),
            1
        );
    }

    #[test]
    fn lossless_ranks_before_free_slot() {
        let mut lossy = candidate("lossy", vec![file("One.mp3", Some(200))]);
        lossy.peer.has_free_slot = Some(true);
        let lossless = candidate("lossless", vec![file("One.flac", Some(200))]);

        let ranked = rank(vec![lossy, lossless]);

        assert_eq!(ranked[0].source, "lossless");
    }

    #[test]
    fn ranking_ties_keep_backend_order() {
        let ranked = rank(vec![
            candidate("first", vec![file("One.flac", Some(200))]),
            candidate("second", vec![file("One.flac", Some(200))]),
        ]);

        assert_eq!(ranked[0].source, "first");
        assert_eq!(ranked[1].source, "second");
    }

    #[test]
    fn the_list_is_cut_to_the_budget() {
        let candidates = (0..5)
            .map(|index| candidate(&format!("c{index}"), vec![file("One.flac", None)]))
            .collect::<Vec<_>>();

        assert_eq!(accepted_for_release(candidates, 1, &[None], 2).len(), 2);
    }
}
