use std::{collections::HashMap, env, fs};

use crate::dedup_fixture::from_ytm;
use serde_json::{Value, json};
use whio_core::features::music_library::domain::{MatchRecord, RecordingIndex};

fn review_dir() -> String {
    format!(
        "{}/../../docs/sessions/spike-dedup/review",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn mode() -> String {
    env::var("WHIO_DEDUP_MODE").unwrap_or_else(|_| "frozen".to_owned())
}

fn paths(mode: &str) -> (String, String, String) {
    let dir = review_dir();
    match mode {
        "homelab" => (
            format!("{dir}/homelab-library.jsonl"),
            format!("{dir}/homelab-ytm-cache.json"),
            format!("{dir}/mb-url-labels.json"),
        ),
        _ => (
            format!("{dir}/data/library.tsv"),
            format!("{dir}/data/ytm_cache.json"),
            format!("{dir}/mb-url-labels.json"),
        ),
    }
}

fn load_library(path: &str) -> Vec<MatchRecord> {
    let content = fs::read_to_string(path).unwrap();
    if path.ends_with(".jsonl") {
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| {
                let row: Value = serde_json::from_str(line).ok()?;
                if row.get("error").is_some() {
                    return None;
                }
                Some(MatchRecord::new(
                    row["id"].as_str()?.to_owned(),
                    row["title"].as_str().unwrap_or_default().to_owned(),
                    row["artists"]
                        .as_array()
                        .map(|artists| {
                            artists
                                .iter()
                                .filter_map(|artist| artist.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                    row["duration_ms"].as_u64(),
                    row["album"].as_str().unwrap_or_default().to_owned(),
                    row["musicbrainz_recording_id"].as_str().map(str::to_owned),
                    row["isrc"].as_str().map(str::to_owned),
                    None,
                    true,
                ))
            })
            .collect()
    } else {
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let columns = line.split('\t').collect::<Vec<_>>();
                MatchRecord::new(
                    columns[0],
                    columns[3],
                    columns[5].split(';').map(str::to_owned).collect(),
                    columns[4].parse::<u64>().ok(),
                    columns[6],
                    (!columns[1].is_empty()).then(|| columns[1].to_owned()),
                    (!columns[2].is_empty()).then(|| columns[2].to_owned()),
                    None,
                    true,
                )
            })
            .collect()
    }
}

fn record_json(record: &MatchRecord) -> Value {
    json!({
        "id": record.id(),
        "title": record.title(),
        "artists": record.artists(),
        "duration_ms": record.duration_ms(),
        "album": record.album(),
        "musicbrainz_recording_id": record.recording_id(),
        "isrc": record.isrc(),
    })
}

fn classify(target: Option<&MatchRecord>, label: Option<&Value>, present: bool) -> &'static str {
    match (target, label) {
        (Some(_), None) => "unlabelled_with_target",
        (None, None) => "unlabelled_no_target",
        (Some(local), Some(label)) => {
            if local.recording_id().is_some()
                && local.recording_id() == label["recording_id"].as_str()
            {
                "identifier_agreement"
            } else {
                "identifier_conflict_with_target"
            }
        }
        (None, Some(_)) if present => "labelled_miss",
        (None, Some(_)) => "labelled_absent_no_target",
    }
}

#[derive(Default, Clone)]
struct Metrics {
    classification: HashMap<String, usize>,
    status: HashMap<String, usize>,
    reason: HashMap<String, usize>,
}

impl Metrics {
    fn to_json(&self) -> Value {
        json!({
            "classification": self.classification,
            "status": self.status,
            "reason": self.reason,
            "precision": null,
            "recall": null,
            "accuracy_note": "identifier conflicts and unlabelled rows remain unresolved; these counts do not establish precision or recall",
        })
    }
}

#[test]
fn a_conflicting_target_is_not_a_miss_or_proven_same_audio() {
    let local = MatchRecord::new(
        "local",
        "Teeth",
        vec!["Artist".into()],
        Some(328000),
        "",
        Some("3090a947-1be6-48c2-b972-455186eae7db".into()),
        None,
        None,
        true,
    );
    let label = json!({"recording_id":"92d54007-370c-4020-a2cc-0e5d55cea3ab", "title":"Hypocrite", "length":248986});
    assert_eq!(
        classify(Some(&local), Some(&label), true),
        "identifier_conflict_with_target"
    );
    assert_eq!(classify(None, Some(&label), true), "labelled_miss");
    let same_metadata = json!({"recording_id":"92d54007-370c-4020-a2cc-0e5d55cea3ab", "title":"Teeth", "length":328000});
    assert_eq!(
        classify(Some(&local), Some(&same_metadata), true),
        "identifier_conflict_with_target"
    );
}

fn bucket(id: &str) -> usize {
    (id.bytes().fold(0u64, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) % 2) as usize
}

#[test]
#[ignore = "reads the local review artifacts"]
fn evaluate() {
    let mode = mode();
    let (library, cache, labels) = paths(&mode);
    let tracks = load_library(&library);
    let by_id = tracks
        .iter()
        .map(|track| (track.id().to_owned(), track.clone()))
        .collect::<HashMap<_, _>>();
    let by_recording = tracks
        .iter()
        .filter_map(|track| {
            track
                .recording_id()
                .map(|recording| (recording.to_owned(), track.id().to_owned()))
        })
        .fold(HashMap::<String, Vec<String>>::new(), |mut map, (r, id)| {
            map.entry(r).or_default().push(id);
            map
        });
    let index = RecordingIndex::new(tracks);
    let labels: Value = fs::read_to_string(&labels)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_else(|| json!({}));
    let cache: Value = serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();

    let mut overall = Metrics::default();
    let mut splits = [Metrics::default(), Metrics::default()];
    let mut rows = Vec::new();
    for (query, entry) in cache.as_object().unwrap() {
        let metrics = &mut splits[bucket(query)];
        for (position, result) in entry["results"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let video = result["videoId"].as_str().unwrap_or_default();
            let decision = index.match_record(&from_ytm(result), 0);
            *overall
                .status
                .entry(decision.status().as_str().to_owned())
                .or_default() += 1;
            *metrics
                .status
                .entry(decision.status().as_str().to_owned())
                .or_default() += 1;
            *overall
                .reason
                .entry(decision.reason().to_owned())
                .or_default() += 1;
            *metrics
                .reason
                .entry(decision.reason().to_owned())
                .or_default() += 1;

            let target = decision.target();
            let label = labels.get(video).filter(|label| !label.is_null());
            let present = label
                .and_then(|label| label["recording_id"].as_str())
                .is_some_and(|id| by_recording.contains_key(id));
            let classification = classify(target.and_then(|id| by_id.get(id)), label, present);
            *overall
                .classification
                .entry(classification.into())
                .or_default() += 1;
            *metrics
                .classification
                .entry(classification.into())
                .or_default() += 1;

            rows.push(json!({
                "query": query,
                "position": position,
                "video_id": video,
                "remote": result,
                "decision": {
                    "status": decision.status().as_str(),
                    "reason": decision.reason(),
                    "target": decision.target(),
                    "candidates": decision.candidates(),
                    "examined": decision.examined(),
                },
                "target_local": target.and_then(|id| by_id.get(id)).map(record_json),
                "oracle": label,
                "classification": classification,
            }));
        }
    }

    let report = json!({
        "mode": mode,
        "library_tracks": by_id.len(),
        "overall": overall.to_json(),
        "split_a": splits[0].to_json(),
        "split_b": splits[1].to_json(),
    });
    let out = format!("{}/dedup-eval-{mode}.json", review_dir());
    fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    if let Ok(dump) = env::var("WHIO_DEDUP_DUMP") {
        let body = rows
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&dump, format!("{body}\n")).unwrap();
    }
    println!("{out}");
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
