use std::{collections::HashMap, env, fs};

use serde_json::Value;
use whio_core::tracks::{MatchRecord, RecordingIndex, from_ytm};

fn review_dir() -> String {
    format!(
        "{}/../../docs/sessions/spike-dedup/review",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
#[ignore = "replays the local review artifacts"]
fn the_port_reproduces_the_python_matcher() {
    let dir = review_dir();
    let library = fs::read_to_string(format!("{dir}/data/library.tsv")).unwrap();
    let mut tracks = Vec::new();
    for line in library.lines().filter(|line| !line.trim().is_empty()) {
        let columns = line.split('\t').collect::<Vec<_>>();
        tracks.push(MatchRecord::new(
            columns[0],
            columns[3],
            columns[5].split(';').map(str::to_owned).collect(),
            columns[4].parse::<u64>().ok(),
            columns[6],
            (!columns[1].is_empty()).then(|| columns[1].to_owned()),
            (!columns[2].is_empty()).then(|| columns[2].to_owned()),
            None,
            true,
        ));
    }
    let index = RecordingIndex::new(tracks);

    let decisions: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(format!("{dir}/decisions.json")).unwrap())
            .unwrap();
    let expected = decisions
        .iter()
        .map(|entry| {
            (
                (
                    entry["query_track"].as_str().unwrap().to_owned(),
                    entry["position"].as_u64().unwrap(),
                ),
                entry["v2"].clone(),
            )
        })
        .collect::<HashMap<_, _>>();

    let cache: Value =
        serde_json::from_str(&fs::read_to_string(format!("{dir}/data/ytm_cache.json")).unwrap())
            .unwrap();
    let mut compared = 0;
    let mut mismatches = Vec::new();
    for (query_track, entry) in cache.as_object().unwrap() {
        for (position, result) in entry["results"].as_array().unwrap().iter().enumerate() {
            let Some(want) = expected.get(&(query_track.clone(), position as u64)) else {
                continue;
            };
            let decision = index.match_record(&from_ytm(result), 0);
            compared += 1;
            let status_matches = want["status"].as_str() == Some(decision.status().as_str());
            let target_matches = want["target"].as_str() == decision.target();
            let reason_matches = want["reason"].as_str() == Some(decision.reason());
            if !(status_matches && target_matches && reason_matches) {
                mismatches.push(serde_json::json!({
                    "query_track": query_track,
                    "position": position,
                    "want": want,
                    "got": {
                        "status": decision.status().as_str(),
                        "reason": decision.reason(),
                        "target": decision.target(),
                    },
                }));
            }
        }
    }

    println!(
        "compared {compared} observations, {} mismatches",
        mismatches.len()
    );
    for mismatch in mismatches.iter().take(10) {
        println!("{}", serde_json::to_string(mismatch).unwrap());
    }
    assert_eq!(compared, 4144);
    assert!(mismatches.is_empty(), "{} mismatches", mismatches.len());
}
