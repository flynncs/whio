use crate::features::music_library::{
    domain::{DuplicateCandidate, EvictionReport, RetentionReport, SplitArtist, SplitArtistRow},
    maintenance::{MaintenanceError, MaintenanceService},
};

pub(crate) async fn duplicates(
    maintenance: &MaintenanceService,
    limit: u32,
) -> Result<(), MaintenanceError> {
    print_duplicates(&maintenance.duplicates(limit).await?);
    print_split_artists(&maintenance.split_artists(limit).await?);
    Ok(())
}

pub(crate) async fn retention(
    maintenance: &MaintenanceService,
    evict: bool,
) -> Result<(), MaintenanceError> {
    print_retention(&maintenance.retention_report().await?);
    if evict {
        print_eviction(&maintenance.evict_unengaged().await?);
    }
    Ok(())
}

fn print_duplicates(candidates: &[DuplicateCandidate]) {
    if candidates.is_empty() {
        println!("no track has more than one local file");
        return;
    }

    let wasted: u64 = candidates
        .iter()
        .map(DuplicateCandidate::wasted_bytes)
        .sum();

    println!(
        "{} tracks hold more than one local file; the copies a keep-rule would retire total {}",
        candidates.len(),
        human_bytes(wasted)
    );

    for candidate in candidates {
        println!(
            "\n{} — {}",
            candidate.artists().join("; "),
            candidate.title()
        );

        let keep = candidate.keep().map(|(source, reason)| {
            println!("  reason: {}", reason.as_str());
            source.source().clone()
        });

        for source in candidate.sources() {
            let marker = if Some(source.source()) == keep.as_ref() {
                "keep  "
            } else {
                "retire"
            };

            println!(
                "  {marker}  {:>10}  {:<6}  {}",
                human_bytes(source.size_bytes()),
                source.suffix().unwrap_or("-"),
                source.path()
            );
        }
    }
}

fn print_split_artists(artists: &[SplitArtist]) {
    if artists.is_empty() {
        return;
    }

    println!(
        "\n{} artist names are held by more than one row",
        artists.len()
    );

    for artist in artists {
        let header = artist
            .rows()
            .first()
            .map_or(artist.name(), SplitArtistRow::name);

        println!("\n{header}");

        for row in artist.rows() {
            println!(
                "  {}  {:<4}  {} tracks, {} albums",
                row.artist_id().as_uuid(),
                if row.is_identified() { "id" } else { "none" },
                row.track_count(),
                row.album_count()
            );
        }
    }

    println!("\nA-046 keeps these rows apart on purpose; merging them is a decision, not a fix.");
}

fn print_retention(report: &RetentionReport) {
    println!(
        "{} tracks, {} albums and {} artists are provider-only and unengaged",
        report.evictable_tracks, report.evictable_albums, report.evictable_artists
    );
    println!(
        "held alive by plays alone: {} tracks, {} albums, {} artists",
        report.play_held_tracks, report.play_held_albums, report.play_held_artists
    );

    match (report.oldest_played_at, report.newest_played_at) {
        (Some(oldest), Some(newest)) => println!(
            "played-only recency: {} .. {}",
            oldest.to_rfc3339(),
            newest.to_rfc3339()
        ),
        _ => println!("played-only recency: none"),
    }

    println!("run `whio-cli retention --evict` to delete the unengaged rows");
}

fn print_eviction(report: &EvictionReport) {
    println!(
        "evicted {} tracks, {} albums and {} artists",
        report.tracks, report.albums, report.artists
    );
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
