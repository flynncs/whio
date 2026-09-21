use std::{collections::HashMap, env, fs};

use serde_json::{Value, json};
use whio_core::tracks::{MatchRecord, ParsedTitle, RecordingIndex, from_ytm};

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

fn same_audio(local: &MatchRecord, label: &Value) -> bool {
    let Some(title) = label.get("title").and_then(Value::as_str) else {
        return false;
    };
    if local.parsed().core() != ParsedTitle::parse(title, &[]).core() {
        return false;
    }
    if let (Some(length), Some(duration)) = (
        label.get("length").and_then(Value::as_u64),
        local.duration_ms(),
    ) && length.abs_diff(duration) > 2000
    {
        return false;
    }

    true
}

#[derive(Default, Clone)]
struct Metrics {
    confirmed_positive: usize,
    confirmed_false_positive: usize,
    false_negative: usize,
    same_audio_other_entity: usize,
    negative_no_target: usize,
    resolved_recording_absent: usize,
    status: HashMap<String, usize>,
    reason: HashMap<String, usize>,
}

impl Metrics {
    fn precision_lower_bound(&self) -> Option<f64> {
        let decided = self.confirmed_positive + self.confirmed_false_positive;
        (decided > 0).then(|| self.confirmed_positive as f64 / decided as f64)
    }

    fn recall_lower_bound(&self) -> Option<f64> {
        let expected = self.confirmed_positive + self.false_negative;
        (expected > 0).then(|| self.confirmed_positive as f64 / expected as f64)
    }

    fn to_json(&self) -> Value {
        json!({
            "confirmed_positive": self.confirmed_positive,
            "confirmed_false_positive": self.confirmed_false_positive,
            "false_negative": self.false_negative,
            "same_audio_other_entity": self.same_audio_other_entity,
            "negative_no_target": self.negative_no_target,
            "resolved_recording_absent": self.resolved_recording_absent,
            "precision_lower_bound": self.precision_lower_bound(),
            "recall_lower_bound": self.recall_lower_bound(),
            "status": self.status,
            "reason": self.reason,
        })
    }
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
            let classification = match label {
                None => {
                    if target.is_some() {
                        "unlabelled_with_target"
                    } else {
                        "unlabelled_no_target"
                    }
                }
                Some(label) => {
                    let recording = label["recording_id"].as_str().unwrap_or_default();
                    match by_recording.get(recording) {
                        Some(ids) if ids.iter().any(|id| Some(id.as_str()) == target) => {
                            overall.confirmed_positive += 1;
                            metrics.confirmed_positive += 1;
                            "confirmed_positive"
                        }
                        Some(_) => {
                            if target
                                .and_then(|id| by_id.get(id))
                                .is_some_and(|local| same_audio(local, label))
                            {
                                overall.same_audio_other_entity += 1;
                                metrics.same_audio_other_entity += 1;
                                "same_audio_other_entity"
                            } else {
                                overall.false_negative += 1;
                                metrics.false_negative += 1;
                                "false_negative"
                            }
                        }
                        None => {
                            overall.resolved_recording_absent += 1;
                            metrics.resolved_recording_absent += 1;
                            match target {
                                None => {
                                    overall.negative_no_target += 1;
                                    metrics.negative_no_target += 1;
                                    "negative_no_target"
                                }
                                Some(target) => {
                                    if by_id
                                        .get(target)
                                        .is_some_and(|local| same_audio(local, label))
                                    {
                                        overall.same_audio_other_entity += 1;
                                        metrics.same_audio_other_entity += 1;
                                        "same_audio_other_entity"
                                    } else {
                                        overall.confirmed_false_positive += 1;
                                        metrics.confirmed_false_positive += 1;
                                        "confirmed_false_positive"
                                    }
                                }
                            }
                        }
                    }
                }
            };

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
