use std::collections::{BTreeMap, BTreeSet, HashMap};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::text::normalize_for_matching;

pub const MATCHER_VERSION: &str = "v2.2";

const MAX_BUCKET: usize = 64;
const MAX_FUZZY: usize = 32;
const DURATION_TOLERANCE_MS: u64 = 2000;
const FUZZY_FLOOR: f64 = 0.9;
const LINK_LIFETIME_SECONDS: i64 = 30 * 86_400;

const VERSION_WORDS: &str = "live remix rmx mix edit acoustic instrumental demo cover karaoke \
    session sessions unplugged alternate bootleg mashup nightcore reprise unpeeled stripped mono \
    stereo clean explicit extended remaster remastered sped slowed rerecorded re-recorded";

const UNDETERMINED_QUALIFIERS: &str = "mix remix demo session sessions cover karaoke";

const ALIASES: &[(&str, &str)] = &[
    ("acoustic version", "acoustic"),
    ("live version", "live"),
    ("remix version", "remix"),
    ("demo version", "demo"),
    ("rmx", "remix"),
    ("re-recorded", "rerecorded"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTitle {
    core: String,
    artists: Vec<String>,
    qualifiers: Vec<String>,
    unknown: Vec<String>,
    unparsed_risk: bool,
}

impl ParsedTitle {
    pub fn parse(title: &str, artists: &[String]) -> Self {
        let full = normalize_for_matching(title);
        let mut credits = artists
            .iter()
            .map(|artist| normalize_for_matching(artist))
            .filter(|artist| !artist.is_empty())
            .collect::<BTreeSet<_>>();
        let mut qualifiers = Vec::new();
        let mut unknown = Vec::new();
        let mut core = strip_brackets(&full, &mut |inner| {
            consume(inner, &mut credits, &mut qualifiers, &mut unknown);
        });

        if let Some((start, suffix)) = find_suffix(&core) {
            let suffix = suffix.to_owned();
            if contains_version_word(&suffix) || feature_credit(&suffix).is_some() {
                consume(&suffix, &mut credits, &mut qualifiers, &mut unknown);
                core.truncate(start);
            }
        }
        if let Some((start, remainder)) = find_feature(&core) {
            let remainder = remainder.to_owned();
            for part in remainder.split(';') {
                let name = normalize_for_matching(part);
                if !name.is_empty() {
                    credits.insert(name);
                }
            }
            core.truncate(start);
        }

        let core = normalize_for_matching(&core);
        let unparsed_risk = contains_version_word(&core);

        Self {
            core,
            artists: credits.into_iter().collect(),
            qualifiers: sorted(qualifiers),
            unknown: sorted(unknown),
            unparsed_risk,
        }
    }

    pub fn core(&self) -> &str {
        &self.core
    }

    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    pub fn qualifiers(&self) -> &[String] {
        &self.qualifiers
    }

    pub fn unknown(&self) -> &[String] {
        &self.unknown
    }

    pub fn unparsed_risk(&self) -> bool {
        self.unparsed_risk
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchRecord {
    id: String,
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
    album: String,
    recording_id: Option<String>,
    isrc: Option<String>,
    explicit: Option<bool>,
    available: bool,
    evidence: String,
    parsed: ParsedTitle,
    fingerprint: String,
}

impl MatchRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        artists: Vec<String>,
        duration_ms: Option<u64>,
        album: impl Into<String>,
        recording_id: Option<String>,
        isrc: Option<String>,
        explicit: Option<bool>,
        available: bool,
    ) -> Self {
        let mut record = Self {
            id: id.into(),
            title: title.into(),
            artists,
            duration_ms: duration_ms.filter(|duration| *duration > 0),
            album: album.into(),
            recording_id: canonical_recording_id(recording_id.as_deref()),
            isrc: canonical_isrc(isrc.as_deref()),
            explicit,
            available,
            evidence: String::new(),
            parsed: ParsedTitle {
                core: String::new(),
                artists: Vec::new(),
                qualifiers: Vec::new(),
                unknown: Vec::new(),
                unparsed_risk: false,
            },
            fingerprint: String::new(),
        };
        record.parsed = ParsedTitle::parse(&record.title, &record.artists);
        record.fingerprint = record.compute_fingerprint();

        record
    }

    pub fn with_provider_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = evidence.into();
        self.fingerprint = self.compute_fingerprint();
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    pub fn album(&self) -> &str {
        &self.album
    }

    pub fn recording_id(&self) -> Option<&str> {
        self.recording_id.as_deref()
    }

    pub fn isrc(&self) -> Option<&str> {
        self.isrc.as_deref()
    }

    pub fn available(&self) -> bool {
        self.available
    }

    pub fn evidence(&self) -> &str {
        &self.evidence
    }

    pub fn parsed(&self) -> &ParsedTitle {
        &self.parsed
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn retrieval_key(&self) -> String {
        loose(&self.parsed.core)
    }

    fn compute_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for part in [
            MATCHER_VERSION,
            &self.id,
            &self.title,
            &self.artists.join("\u{1f}"),
            &self
                .duration_ms
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            &self.album,
            self.recording_id.as_deref().unwrap_or_default(),
            self.isrc.as_deref().unwrap_or_default(),
            &self
                .explicit
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            &self.available.to_string(),
            &self.evidence,
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }

        format!("{:x}", hasher.finalize())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStatus {
    Verified,
    MetadataCandidate,
    Review,
    Abstain,
    NoMatch,
}

impl MatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::MetadataCandidate => "metadata_candidate",
            Self::Review => "review",
            Self::Abstain => "abstain",
            Self::NoMatch => "no_match",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchDecision {
    status: MatchStatus,
    reason: &'static str,
    target: Option<String>,
    candidates: Vec<String>,
    examined: usize,
}

impl MatchDecision {
    fn new(
        status: MatchStatus,
        reason: &'static str,
        target: Option<String>,
        candidates: Vec<String>,
        examined: usize,
    ) -> Self {
        Self {
            status,
            reason,
            target,
            candidates,
            examined,
        }
    }

    pub fn status(&self) -> MatchStatus {
        self.status
    }

    pub fn reason(&self) -> &'static str {
        self.reason
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    pub fn examined(&self) -> usize {
        self.examined
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedLink {
    target: String,
    remote_fingerprint: String,
    local_fingerprint: String,
    version: String,
    expires: i64,
}

#[derive(Default)]
pub struct RecordingIndex {
    tracks: BTreeMap<String, MatchRecord>,
    by_core: BTreeMap<String, Vec<String>>,
    by_artists: BTreeMap<Vec<String>, Vec<String>>,
    by_recording_id: BTreeMap<String, Vec<String>>,
    by_isrc: BTreeMap<String, Vec<String>>,
    verified: BTreeMap<String, VerifiedLink>,
}

impl RecordingIndex {
    pub fn new(tracks: Vec<MatchRecord>) -> Self {
        let mut index = Self::default();
        let mut tracks = tracks;
        tracks.sort_by(|left, right| left.id.cmp(&right.id));
        for track in tracks {
            let id = track.id.clone();
            index
                .by_core
                .entry(loose(track.parsed.core()))
                .or_default()
                .push(id.clone());
            index
                .by_artists
                .entry(track.parsed.artists().to_vec())
                .or_default()
                .push(id.clone());
            if let Some(recording_id) = track.recording_id().map(str::to_owned) {
                index
                    .by_recording_id
                    .entry(recording_id)
                    .or_default()
                    .push(id.clone());
            }
            if let Some(isrc) = track.isrc().map(str::to_owned) {
                index.by_isrc.entry(isrc).or_default().push(id.clone());
            }
            index.tracks.insert(id, track);
        }

        index
    }

    pub fn confirm(
        &mut self,
        remote: &MatchRecord,
        local_id: &str,
        provenance: &str,
        now: i64,
    ) -> Result<(), &'static str> {
        if remote.id.is_empty() || provenance.trim().is_empty() {
            return Err("verification_requires_video_id_and_provenance");
        }
        let Some(local) = self.tracks.get(local_id) else {
            return Err("unknown_local");
        };
        if !local.available || conflicts(remote, local).is_some() {
            return Err("conflicting_evidence");
        }
        self.verified.insert(
            remote.id.clone(),
            VerifiedLink {
                target: local_id.to_owned(),
                remote_fingerprint: remote.fingerprint.clone(),
                local_fingerprint: local.fingerprint.clone(),
                version: MATCHER_VERSION.to_owned(),
                expires: now + LINK_LIFETIME_SECONDS,
            },
        );

        Ok(())
    }

    pub fn match_record(&self, remote: &MatchRecord, now: i64) -> MatchDecision {
        if let Some(link) = self.verified.get(&remote.id)
            && let Some(local) = self.tracks.get(&link.target)
            && local.available
            && link.version == MATCHER_VERSION
            && link.expires > now
            && link.remote_fingerprint == remote.fingerprint
            && link.local_fingerprint == local.fingerprint
        {
            return MatchDecision::new(
                MatchStatus::Verified,
                "verified_video_id",
                Some(local.id.clone()),
                vec![local.id.clone()],
                1,
            );
        }

        let mut by_id = BTreeSet::new();
        if let Some(recording_id) = remote.recording_id()
            && let Some(ids) = self.by_recording_id.get(recording_id)
        {
            by_id.extend(ids.iter().cloned());
        }
        if let Some(isrc) = remote.isrc()
            && let Some(ids) = self.by_isrc.get(isrc)
        {
            by_id.extend(ids.iter().cloned());
        }
        let pool = if by_id.is_empty() {
            self.by_core
                .get(&loose(remote.parsed.core()))
                .cloned()
                .unwrap_or_default()
        } else {
            by_id.into_iter().collect::<Vec<_>>()
        };

        if pool.len() > MAX_BUCKET {
            return MatchDecision::new(
                MatchStatus::Abstain,
                "candidate_budget",
                None,
                Vec::new(),
                0,
            );
        }

        let mut accepted = Vec::new();
        let mut reasons: Vec<(&'static str, usize)> = Vec::new();
        for id in &pool {
            match metadata_gate(remote, &self.tracks[id], DURATION_TOLERANCE_MS) {
                Some(reason) => increment(&mut reasons, reason),
                None => accepted.push(id.clone()),
            }
        }

        if !accepted.is_empty() {
            let identities = accepted
                .iter()
                .map(|id| {
                    self.tracks[id]
                        .recording_id()
                        .map_or_else(|| format!("track:{id}"), str::to_owned)
                })
                .collect::<BTreeSet<_>>();
            if identities.len() > 1 {
                return MatchDecision::new(
                    MatchStatus::Abstain,
                    "ambiguous_recordings",
                    None,
                    accepted,
                    pool.len(),
                );
            }
            let target = accepted.into_iter().min().expect("accepted is not empty");
            let shared = remote.recording_id().is_some()
                && remote.recording_id() == self.tracks[&target].recording_id();
            let (status, reason) = if shared {
                (MatchStatus::Verified, "shared_recording_id")
            } else {
                (MatchStatus::MetadataCandidate, "strict_metadata")
            };

            return MatchDecision::new(status, reason, Some(target), Vec::new(), pool.len());
        }

        if !pool.is_empty() {
            return MatchDecision::new(
                MatchStatus::Abstain,
                most_common(&reasons),
                None,
                pool.clone(),
                pool.len(),
            );
        }

        let artist_pool = self
            .by_artists
            .get(remote.parsed.artists())
            .cloned()
            .unwrap_or_default();
        if remote.parsed.artists().is_empty() || artist_pool.len() > MAX_FUZZY {
            return MatchDecision::new(
                MatchStatus::NoMatch,
                "no_exact_candidate",
                None,
                Vec::new(),
                0,
            );
        }

        let mut suggestions = Vec::new();
        for id in &artist_pool {
            let local = &self.tracks[id];
            let (Some(remote_duration), Some(local_duration)) =
                (remote.duration_ms(), local.duration_ms())
            else {
                continue;
            };
            if !local.available
                || conflicts(remote, local).is_some()
                || remote_duration.abs_diff(local_duration) > DURATION_TOLERANCE_MS
            {
                continue;
            }
            let score = sequence_ratio(remote.parsed.core(), local.parsed.core());
            if score >= FUZZY_FLOOR {
                suggestions.push((score, id.clone()));
            }
        }
        suggestions.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        let candidates = suggestions
            .into_iter()
            .take(3)
            .map(|(_, id)| id)
            .collect::<Vec<_>>();

        if candidates.is_empty() {
            MatchDecision::new(
                MatchStatus::NoMatch,
                "no_exact_candidate",
                None,
                Vec::new(),
                artist_pool.len(),
            )
        } else {
            MatchDecision::new(
                MatchStatus::Review,
                "fuzzy_suggestion",
                None,
                candidates,
                artist_pool.len(),
            )
        }
    }
}

pub fn from_ytm(raw: &serde_json::Value) -> MatchRecord {
    let artists = raw
        .get("artists")
        .and_then(serde_json::Value::as_array)
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| artist.get("name").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let seconds = raw
        .get("duration_seconds")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| {
            raw.get("duration")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_clock_seconds)
        });

    MatchRecord::new(
        raw.get("videoId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        raw.get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        artists,
        seconds.map(|seconds| (seconds * 1000.0).round() as u64),
        raw.get("album")
            .and_then(|album| album.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        raw.get("musicbrainz_recording_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        raw.get("isrc")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        raw.get("isExplicit").and_then(serde_json::Value::as_bool),
        true,
    )
    .with_provider_evidence(raw.to_string())
}

fn parse_clock_seconds(value: &str) -> Option<f64> {
    let parts = value
        .split(':')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if !(2..=3).contains(&parts.len()) || parts[1..].iter().any(|part| *part >= 60) {
        return None;
    }

    Some(
        parts
            .iter()
            .rev()
            .enumerate()
            .map(|(power, part)| *part as f64 * 60f64.powi(power as i32))
            .sum(),
    )
}

fn canonical_recording_id(value: Option<&str>) -> Option<String> {
    Uuid::parse_str(value?).ok().map(|id| id.to_string())
}

fn canonical_isrc(value: Option<&str>) -> Option<String> {
    let normalized = value?
        .chars()
        .filter(|character| *character != '-' && !character.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    let bytes = normalized.as_bytes();
    let valid = bytes.len() == 12
        && bytes[..2].iter().all(|byte| byte.is_ascii_uppercase())
        && bytes[2..5]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes[5..].iter().all(|byte| byte.is_ascii_digit());

    valid.then_some(normalized)
}

fn conflicts(left: &MatchRecord, right: &MatchRecord) -> Option<&'static str> {
    if left.recording_id().is_some()
        && right.recording_id().is_some()
        && left.recording_id() != right.recording_id()
    {
        return Some("recording_id_conflict");
    }
    if left.isrc().is_some() && right.isrc().is_some() && left.isrc() != right.isrc() {
        return Some("isrc_conflict");
    }
    if let (Some(left), Some(right)) = (left.explicit, right.explicit)
        && left != right
    {
        return Some("explicit_conflict");
    }
    if left.parsed.qualifiers != right.parsed.qualifiers {
        return Some("version_or_qualifier_conflict");
    }

    None
}

fn metadata_gate(
    remote: &MatchRecord,
    local: &MatchRecord,
    tolerance_ms: u64,
) -> Option<&'static str> {
    if !local.available {
        return Some("local_unavailable");
    }
    if remote.parsed.artists.is_empty() || local.parsed.artists.is_empty() {
        return Some("artist_credits_differ");
    }
    if remote.parsed.artists != local.parsed.artists {
        if !is_strict_subset(&local.parsed.artists, &remote.parsed.artists) {
            return Some("artist_credits_differ");
        }

        return feature_superset_gate(remote, local, 1000);
    }
    if remote.parsed.core.is_empty() || remote.parsed.core != local.parsed.core {
        return Some("title_not_exact");
    }
    if let Some(issue) = conflicts(remote, local) {
        return Some(issue);
    }
    let shared = remote.recording_id().is_some() && remote.recording_id() == local.recording_id();

    if (remote.parsed.unparsed_risk || local.parsed.unparsed_risk) && !shared {
        return Some("unparsed_version_wording");
    }
    if remote
        .parsed
        .qualifiers
        .iter()
        .any(|q| contains_word(q, "live"))
        && !shared
    {
        return Some("live_needs_verification");
    }
    if remote.parsed.qualifiers.iter().any(|q| {
        UNDETERMINED_QUALIFIERS
            .split_whitespace()
            .any(|word| word == q)
    }) && !shared
    {
        return Some("underspecified_version");
    }

    match (remote.duration_ms, local.duration_ms) {
        (Some(remote), Some(local)) if remote.abs_diff(local) > tolerance_ms => {
            Some("duration_conflict")
        }
        (None, _) | (_, None) => (!shared).then_some("missing_duration"),
        _ => None,
    }
}

fn feature_superset_gate(
    remote: &MatchRecord,
    local: &MatchRecord,
    tolerance_ms: u64,
) -> Option<&'static str> {
    if remote.parsed.core.is_empty() || remote.parsed.core != local.parsed.core {
        return Some("title_not_exact");
    }
    if let Some(issue) = conflicts(remote, local) {
        return Some(issue);
    }
    let shared = remote.recording_id().is_some() && remote.recording_id() == local.recording_id();
    if (remote.parsed.unparsed_risk || local.parsed.unparsed_risk) && !shared {
        return Some("unparsed_version_wording");
    }

    match (remote.duration_ms, local.duration_ms) {
        (Some(remote), Some(local)) if remote.abs_diff(local) > tolerance_ms => {
            Some("duration_conflict")
        }
        (None, _) | (_, None) => (!shared).then_some("missing_duration"),
        _ => None,
    }
}

fn is_strict_subset(local: &[String], remote: &[String]) -> bool {
    local.len() < remote.len() && local.iter().all(|artist| remote.contains(artist))
}

fn increment(reasons: &mut Vec<(&'static str, usize)>, reason: &'static str) {
    match reasons.iter_mut().find(|(name, _)| *name == reason) {
        Some((_, count)) => *count += 1,
        None => reasons.push((reason, 1)),
    }
}

fn most_common(reasons: &[(&'static str, usize)]) -> &'static str {
    reasons
        .iter()
        .max_by_key(|(_, count)| *count)
        .map_or("no_exact_candidate", |(reason, _)| *reason)
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

fn alias(value: &str) -> &str {
    ALIASES
        .iter()
        .find(|(from, _)| *from == value)
        .map_or(value, |(_, to)| to)
}

fn consume(
    content: &str,
    credits: &mut BTreeSet<String>,
    qualifiers: &mut Vec<String>,
    unknown: &mut Vec<String>,
) {
    let normalized = normalize_for_matching(content);
    if let Some(rest) = feature_credit(&normalized) {
        credits.extend(
            rest.split(';')
                .map(normalize_for_matching)
                .filter(|name| !name.is_empty()),
        );
        return;
    }

    let qualifier = alias(&normalized).to_owned();
    if !contains_version_word(&qualifier) {
        unknown.push(qualifier.clone());
    }
    qualifiers.push(qualifier);
}

fn feature_credit(value: &str) -> Option<&str> {
    ["feat", "ft", "featuring"].iter().find_map(|keyword| {
        let rest = value.strip_prefix(keyword)?;
        let rest = rest.strip_prefix('.').unwrap_or(rest);
        rest.strip_prefix(' ').filter(|rest| !rest.is_empty())
    })
}

fn strip_brackets(value: &str, consume: &mut impl FnMut(&str)) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut result = String::with_capacity(value.len());
    let mut index = 0;

    while index < characters.len() {
        let (open, close) = match characters[index] {
            '(' => ('(', ')'),
            '[' => ('[', ']'),
            '{' => ('{', '}'),
            other => {
                result.push(other);
                index += 1;
                continue;
            }
        };
        let closing = (index + 1..characters.len())
            .take_while(|cursor| characters[*cursor] != open)
            .find(|cursor| characters[*cursor] == close);

        match closing {
            Some(closing) => {
                consume(&characters[index + 1..closing].iter().collect::<String>());
                result.push(' ');
                index = closing + 1;
            }
            None => {
                result.push(characters[index]);
                index += 1;
            }
        }
    }

    result
}

fn find_suffix(value: &str) -> Option<(usize, &str)> {
    let bytes = value.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_whitespace() {
            continue;
        }
        let mut cursor = start;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'-') {
            continue;
        }
        let mut after = cursor + 1;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if after > cursor + 1 && after < bytes.len() {
            return Some((start, &value[after..]));
        }
    }

    None
}

fn find_feature(value: &str) -> Option<(usize, &str)> {
    let bytes = value.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_whitespace() {
            continue;
        }
        let mut cursor = start;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        for keyword in ["feat", "ft", "featuring"] {
            let Some(rest) = value[cursor..].strip_prefix(keyword) else {
                continue;
            };
            let rest = rest.strip_prefix('.').unwrap_or(rest);
            let Some(rest) = rest.strip_prefix(' ') else {
                continue;
            };
            let offset = value.len() - rest.len();
            if offset < value.len() {
                return Some((start, rest));
            }
        }
    }

    None
}

fn loose(value: &str) -> String {
    normalize_for_matching(value)
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '_' {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn contains_word(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(index) = text[start..].find(word) {
        let at = start + index;
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        if before.is_none_or(|c| !is_word_character(c))
            && after.is_none_or(|c| !is_word_character(c))
        {
            return true;
        }
        start = at + word.len();
    }

    false
}

fn contains_version_word(text: &str) -> bool {
    VERSION_WORDS
        .split_whitespace()
        .any(|word| contains_word(text, word))
}

fn sequence_ratio(left: &str, right: &str) -> f64 {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let total = left.len() + right.len();
    if total == 0 {
        return 1.0;
    }

    let mut positions: HashMap<char, Vec<usize>> = HashMap::new();
    for (index, character) in right.iter().enumerate() {
        positions.entry(*character).or_default().push(index);
    }

    let mut queue = vec![(0usize, left.len(), 0usize, right.len())];
    let mut matched = 0;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (start_left, start_right, size) = longest_match(&left, &positions, alo, ahi, blo, bhi);
        if size == 0 {
            continue;
        }
        matched += size;
        if alo < start_left && blo < start_right {
            queue.push((alo, start_left, blo, start_right));
        }
        if start_left + size < ahi && start_right + size < bhi {
            queue.push((start_left + size, ahi, start_right + size, bhi));
        }
    }

    (2.0 * matched as f64) / total as f64
}

fn longest_match(
    left: &[char],
    positions: &HashMap<char, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut best_left, mut best_right, mut best_size) = (alo, blo, 0);
    let mut previous: HashMap<usize, usize> = HashMap::new();

    for (index, character) in left.iter().enumerate().take(ahi).skip(alo) {
        let mut current: HashMap<usize, usize> = HashMap::new();
        for &position in positions.get(character).into_iter().flatten() {
            if position < blo {
                continue;
            }
            if position >= bhi {
                break;
            }
            let length = position
                .checked_sub(1)
                .and_then(|earlier| previous.get(&earlier))
                .copied()
                .unwrap_or(0)
                + 1;
            current.insert(position, length);
            if length > best_size {
                best_left = index + 1 - length;
                best_right = position + 1 - length;
                best_size = length;
            }
        }
        previous = current;
    }

    (best_left, best_right, best_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Clone)]
    struct Fixture {
        name: &'static str,
        expected: bool,
        local_title: &'static str,
        remote_title: &'static str,
        local_artists: &'static [&'static str],
        remote_artists: &'static [&'static str],
        local_explicit: Option<bool>,
        remote_explicit: Option<bool>,
        local_mbid: Option<&'static str>,
        remote_mbid: Option<&'static str>,
        local_isrc: Option<&'static str>,
        remote_isrc: Option<&'static str>,
        remote_duration_ms: u64,
        remote_album: &'static str,
        extra_local: bool,
        available: bool,
    }

    const MBID_A: &str = "11111111-1111-1111-1111-111111111111";
    const MBID_B: &str = "22222222-2222-2222-2222-222222222222";

    fn fixture(name: &'static str, expected: bool) -> Fixture {
        Fixture {
            name,
            expected,
            local_title: "Song",
            remote_title: "Song",
            local_artists: &["A"],
            remote_artists: &["A"],
            local_explicit: None,
            remote_explicit: None,
            local_mbid: None,
            remote_mbid: None,
            local_isrc: None,
            remote_isrc: None,
            remote_duration_ms: 240_000,
            remote_album: "Album",
            extra_local: false,
            available: true,
        }
    }

    fn fixtures() -> Vec<Fixture> {
        vec![
            fixture("exact", true),
            Fixture {
                local_title: "  SONG ",
                remote_title: "song",
                ..fixture("case and whitespace", true)
            },
            Fixture {
                local_title: "It's Over",
                remote_title: "It\u{2019}s Over",
                ..fixture("curly apostrophe", true)
            },
            Fixture {
                local_title: "Café",
                remote_title: "Cafe\u{0301}",
                ..fixture("unicode NFC", true)
            },
            Fixture {
                local_title: "Song (Acoustic)",
                remote_title: "Song - Acoustic Version",
                ..fixture("acoustic delimiters", true)
            },
            Fixture {
                local_artists: &["A", "B"],
                remote_artists: &["B", "A"],
                ..fixture("reordered credits", true)
            },
            Fixture {
                remote_title: "Song (feat. B)",
                local_artists: &["A", "B"],
                ..fixture("featured credit parsed", true)
            },
            Fixture {
                remote_album: "Greatest Hits",
                ..fixture("compilation album differs", true)
            },
            Fixture {
                local_title: "Song (About You)",
                remote_title: "Song (About You)",
                ..fixture("literal bracketed title unchanged", true)
            },
            Fixture {
                remote_duration_ms: 241_000,
                ..fixture("duration rounding", true)
            },
            Fixture {
                local_title: "Song (Live at Wembley)",
                ..fixture("studio remote against local live", false)
            },
            Fixture {
                remote_title: "Song (Live at Wembley)",
                ..fixture("live remote against local studio", false)
            },
            Fixture {
                local_title: "Song (Live at Wembley)",
                remote_title: "Song (Live at Paris)",
                ..fixture("different live venue", false)
            },
            Fixture {
                local_title: "Song (Live)",
                remote_title: "Song (Live)",
                ..fixture("same underspecified live", false)
            },
            Fixture {
                local_title: "Song (Alpha Remix)",
                remote_title: "Song (Beta Remix)",
                ..fixture("different named remix", false)
            },
            Fixture {
                local_title: "Song (Alpha Remix)",
                remote_title: "Song - Alpha Remix",
                ..fixture("same named remix", true)
            },
            Fixture {
                local_title: "Song (Remix)",
                remote_title: "Song (Remix)",
                ..fixture("same generic remix", false)
            },
            Fixture {
                local_title: "Song (Acoustic)",
                ..fixture("local acoustic against studio", false)
            },
            Fixture {
                local_title: "Song (Radio Edit)",
                ..fixture("radio edit vs studio", false)
            },
            Fixture {
                remote_title: "Song (Sunset Session)",
                ..fixture("unknown qualifier remote", false)
            },
            Fixture {
                local_title: "Song (Blue Room)",
                ..fixture("unknown qualifier local", false)
            },
            Fixture {
                local_title: "Song (Blue Room)",
                remote_title: "Song (Red Room)",
                ..fixture("different unknown qualifier", false)
            },
            Fixture {
                local_title: "Song (Clean)",
                remote_title: "Song (Explicit)",
                ..fixture("clean explicit title conflict", false)
            },
            Fixture {
                local_explicit: Some(false),
                remote_explicit: Some(true),
                ..fixture("clean explicit field conflict", false)
            },
            Fixture {
                local_title: "Song (Pt. 1)",
                remote_title: "Song (Pt. 2)",
                ..fixture("parts are significant", false)
            },
            Fixture {
                local_title: "Song (Mono)",
                remote_title: "Song (Stereo)",
                ..fixture("mono stereo conflict", false)
            },
            Fixture {
                remote_title: "Song (2013 Remaster)",
                ..fixture("remaster needs evidence", false)
            },
            Fixture {
                local_artists: &["Johnny Cash"],
                remote_artists: &["Nine Inch Nails"],
                ..fixture("different cover artist", false)
            },
            Fixture {
                local_artists: &["Artist A"],
                remote_artists: &["Artist B"],
                ..fixture("similar artist names", false)
            },
            Fixture {
                remote_artists: &[],
                ..fixture("missing artist", false)
            },
            Fixture {
                local_artists: &["A", "B"],
                remote_artists: &["A"],
                ..fixture("dropped featured artist", false)
            },
            Fixture {
                local_title: "Song With You",
                remote_title: "Song With Me",
                ..fixture("with is title text", false)
            },
            Fixture {
                local_title: "A+B",
                remote_title: "A B",
                ..fixture("punctuation collision", false)
            },
            Fixture {
                remote_duration_ms: 245_000,
                ..fixture("duration exceeds hard gate", false)
            },
            Fixture {
                remote_duration_ms: 0,
                ..fixture("missing duration", false)
            },
            Fixture {
                extra_local: true,
                ..fixture("multiple recordings same metadata", false)
            },
            Fixture {
                available: false,
                ..fixture("unavailable local", false)
            },
            Fixture {
                local_mbid: Some(MBID_A),
                remote_mbid: Some(MBID_B),
                local_isrc: Some("USAAA2400001"),
                remote_isrc: Some("USAAA2400001"),
                ..fixture("shared ISRC conflicting MBIDs", false)
            },
            Fixture {
                local_mbid: Some(MBID_A),
                remote_mbid: Some(MBID_A),
                ..fixture("same recording identifier", true)
            },
            Fixture {
                local_title: "The Long Goodbye",
                remote_title: "The Long Goodby",
                ..fixture("typo is review only", false)
            },
            Fixture {
                remote_duration_ms: 0,
                local_mbid: Some(MBID_A),
                remote_mbid: Some(MBID_A),
                ..fixture("shared recording ID supports missing duration", true)
            },
            Fixture {
                local_title: "Song (Live)",
                remote_title: "Song (Live)",
                local_mbid: Some(MBID_A),
                remote_mbid: Some(MBID_A),
                ..fixture("shared recording ID supports live identity", true)
            },
        ]
    }

    fn local_record(case: &Fixture, id: &str) -> MatchRecord {
        MatchRecord::new(
            id,
            case.local_title,
            case.local_artists.iter().map(|a| (*a).to_owned()).collect(),
            Some(240_000),
            "Album",
            case.local_mbid.map(str::to_owned),
            case.local_isrc.map(str::to_owned),
            case.local_explicit,
            case.available,
        )
    }

    fn remote_record(case: &Fixture) -> MatchRecord {
        MatchRecord::new(
            "remote",
            case.remote_title,
            case.remote_artists
                .iter()
                .map(|a| (*a).to_owned())
                .collect(),
            Some(case.remote_duration_ms),
            case.remote_album,
            case.remote_mbid.map(str::to_owned),
            case.remote_isrc.map(str::to_owned),
            case.remote_explicit,
            true,
        )
    }

    #[test]
    fn the_forty_two_behaviour_cases_hold() {
        let cases = fixtures();
        assert_eq!(cases.len(), 42);
        for case in cases {
            let mut tracks = vec![local_record(&case, "local")];
            if case.extra_local {
                tracks.push(local_record(&case, "other-local"));
            }
            let decision = RecordingIndex::new(tracks).match_record(&remote_record(&case), 0);
            assert_eq!(
                decision.target().is_some(),
                case.expected,
                "{}: {:?}",
                case.name,
                decision
            );
        }
    }

    #[test]
    fn a_verification_link_expires_and_invalidates() {
        let case = fixture("exact", true);
        let remote = remote_record(&case);
        let mut index = RecordingIndex::new(vec![local_record(&case, "local")]);
        index
            .confirm(&remote, "local", "synthetic independent verification", 100)
            .unwrap();

        assert_eq!(
            index.match_record(&remote, 101).reason(),
            "verified_video_id"
        );
        assert_ne!(
            index
                .match_record(&remote, 100 + LINK_LIFETIME_SECONDS + 1)
                .reason(),
            "verified_video_id"
        );

        let relocated = MatchRecord::new(
            "remote",
            "Song",
            vec!["A".to_owned()],
            Some(250_000),
            "Album",
            None,
            None,
            None,
            true,
        );
        assert_ne!(
            index.match_record(&relocated, 101).reason(),
            "verified_video_id"
        );

        let revised = RecordingIndex::new(vec![MatchRecord::new(
            "local",
            "Different",
            vec!["A".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            true,
        )]);
        let mut revised = revised;
        revised.verified = index.verified.clone();
        assert_ne!(
            revised.match_record(&remote, 101).reason(),
            "verified_video_id"
        );

        let mut unavailable = RecordingIndex::new(vec![MatchRecord::new(
            "local",
            "Song",
            vec!["A".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            false,
        )]);
        unavailable.verified = index.verified.clone();
        assert_ne!(
            unavailable.match_record(&remote, 101).reason(),
            "verified_video_id"
        );

        assert_ne!(
            RecordingIndex::new(Vec::new())
                .match_record(&remote, 101)
                .reason(),
            "verified_video_id"
        );

        let changed = remote
            .clone()
            .with_provider_evidence("provider artist id changed");
        assert_ne!(
            index.match_record(&changed, 101).reason(),
            "verified_video_id"
        );

        index.verified.get_mut("remote").unwrap().version = "old".to_owned();
        assert_ne!(
            index.match_record(&remote, 101).reason(),
            "verified_video_id"
        );
    }

    #[test]
    fn a_remote_feature_superset_is_accepted_but_a_dropped_credit_is_not() {
        let single = MatchRecord::new(
            "local",
            "Song",
            vec!["A".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            true,
        );
        let pair = MatchRecord::new(
            "local",
            "Song",
            vec!["A".to_owned(), "B".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            true,
        );
        let superset = MatchRecord::new(
            "remote",
            "Song",
            vec!["A".to_owned(), "B".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            true,
        );
        let dropped = MatchRecord::new(
            "remote",
            "Song",
            vec!["A".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            true,
        );
        let close = MatchRecord::new(
            "remote",
            "Song",
            vec!["A".to_owned(), "B".to_owned()],
            Some(240_500),
            "Album",
            None,
            None,
            None,
            true,
        );
        let far = MatchRecord::new(
            "remote",
            "Song",
            vec!["A".to_owned(), "B".to_owned()],
            Some(242_500),
            "Album",
            None,
            None,
            None,
            true,
        );

        let superset_index = RecordingIndex::new(vec![single.clone()]);
        assert_eq!(
            superset_index.match_record(&superset, 0).target(),
            Some("local")
        );
        assert_eq!(
            superset_index.match_record(&close, 0).target(),
            Some("local")
        );
        assert!(superset_index.match_record(&far, 0).target().is_none());

        let dropped_index = RecordingIndex::new(vec![pair]);
        assert!(dropped_index.match_record(&dropped, 0).target().is_none());
    }

    #[test]
    fn fuzzy_suggestions_are_bounded_and_order_independent() {
        let tracks = (0..100)
            .map(|index| {
                MatchRecord::new(
                    index.to_string(),
                    format!("The Long Goodbye {index}"),
                    vec!["A".to_owned()],
                    Some(240_000),
                    "Album",
                    None,
                    None,
                    None,
                    true,
                )
            })
            .collect::<Vec<_>>();
        let remote = MatchRecord::new(
            "remote",
            "The Long Goodby",
            vec!["A".to_owned()],
            Some(240_000),
            "Album",
            None,
            None,
            None,
            true,
        );
        let decision = RecordingIndex::new(tracks).match_record(&remote, 0);
        assert_eq!(decision.examined(), 0);
        assert!(decision.target().is_none());

        let case = fixtures()
            .into_iter()
            .find(|case| case.extra_local)
            .unwrap();
        let forward = RecordingIndex::new(vec![
            local_record(&case, "local"),
            local_record(&case, "other-local"),
        ])
        .match_record(&remote_record(&case), 0);
        let reverse = RecordingIndex::new(vec![
            local_record(&case, "other-local"),
            local_record(&case, "local"),
        ])
        .match_record(&remote_record(&case), 0);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn provider_evidence_and_clock_durations_survive() {
        let raw = json!({
            "videoId": "abc",
            "title": "Song",
            "artists": [{"name": "A", "id": "artist-id"}],
            "album": {"name": "Album", "id": "album-id"},
            "isExplicit": true,
            "duration": "4:00"
        });
        let track = from_ytm(&raw);
        assert_eq!(track.duration_ms(), Some(240_000));
        assert!(track.evidence().contains("artist-id"));
        assert!(track.evidence().contains("album-id"));
    }
}
