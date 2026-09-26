use whio_core::features::music_library::domain::MatchRecord;

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
