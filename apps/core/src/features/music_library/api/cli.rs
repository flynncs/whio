use crate::features::music_library::{
    domain::{
        DuplicateCandidate, DuplicateKind, EvictionReport, ProviderDuplicateReport,
        RetentionReport, SkipReason, SplitArtist, SplitArtistRow,
    },
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

pub(crate) async fn prune_provider_duplicates(
    maintenance: &MaintenanceService,
    apply: bool,
) -> Result<(), MaintenanceError> {
    let report = if apply {
        maintenance.delete_provider_duplicates().await?
    } else {
        maintenance.provider_duplicates().await?
    };
    print_provider_duplicate_report(&report, apply);
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

fn print_provider_duplicate_report(report: &ProviderDuplicateReport, applied: bool) {
    print!("{}", format_provider_duplicate_report(report, applied));
}

fn format_provider_duplicate_report(report: &ProviderDuplicateReport, applied: bool) -> String {
    let mut output = String::new();
    for merge in &report.merges {
        output.push_str(&format!(
            "merge artist {} \"{}\" -> {} credits={} albums_moved={} albums_merged={}\n",
            merge.id,
            merge.name,
            merge.into,
            merge.credits,
            merge.albums_moved,
            merge.albums_merged
        ));
    }
    for duplicate in &report.duplicates {
        output.push_str(&format!(
            "duplicate {} {} {:?} -> {} listens={}\n",
            duplicate_kind(duplicate.kind),
            duplicate.id,
            duplicate.name,
            duplicate.duplicate_of,
            duplicate.listens
        ));
    }
    for skipped in &report.skipped {
        output.push_str(&format!(
            "skipped {} {} {:?} {}\n",
            duplicate_kind(skipped.kind),
            skipped.id,
            skipped.name,
            skip_reason(skipped.reason)
        ));
    }
    let mut tracks = 0;
    let mut albums = 0;
    let mut artists = 0;
    for duplicate in &report.duplicates {
        match duplicate.kind {
            DuplicateKind::Track => tracks += 1,
            DuplicateKind::Album => albums += 1,
            DuplicateKind::Artist => artists += 1,
        }
    }
    output.push_str(&format!(
        "{}: merges={} tracks={tracks} albums={albums} artists={artists} skipped={}\n",
        if applied { "deleted" } else { "would delete" },
        report.merges.len(),
        report.skipped.len()
    ));
    output
}

fn duplicate_kind(kind: DuplicateKind) -> &'static str {
    match kind {
        DuplicateKind::Track => "track",
        DuplicateKind::Album => "album",
        DuplicateKind::Artist => "artist",
    }
}

fn skip_reason(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::PlaylistEntry => "playlist_entry",
        SkipReason::QueueEntry => "queue_entry",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::music_library::domain::{
        ArtistMerge, ProviderDuplicate, ProviderDuplicateReport, SkipReason, SkippedDuplicate,
    };
    use uuid::Uuid;

    #[test]
    fn prune_without_apply_prints_would_delete() {
        let report = ProviderDuplicateReport {
            merges: vec![],
            duplicates: vec![ProviderDuplicate {
                kind: DuplicateKind::Track,
                id: Uuid::nil(),
                name: "Glue Song".to_owned(),
                duplicate_of: Uuid::from_u128(1),
                listens: 3,
            }],
            skipped: vec![SkippedDuplicate {
                kind: DuplicateKind::Track,
                id: Uuid::from_u128(2),
                name: "Queue Song".to_owned(),
                reason: SkipReason::QueueEntry,
            }],
        };

        assert_eq!(
            format_provider_duplicate_report(&report, false),
            "duplicate track 00000000-0000-0000-0000-000000000000 \"Glue Song\" -> 00000000-0000-0000-0000-000000000001 listens=3\nskipped track 00000000-0000-0000-0000-000000000002 \"Queue Song\" queue_entry\nwould delete: merges=0 tracks=1 albums=0 artists=0 skipped=1\n"
        );
    }

    #[test]
    fn prune_prints_merges_before_duplicates() {
        let report = ProviderDuplicateReport {
            merges: vec![ArtistMerge {
                id: Uuid::from_u128(1),
                name: "beabadoobee".to_owned(),
                into: Uuid::from_u128(2),
                credits: 14,
                albums_moved: 35,
                albums_merged: 1,
            }],
            duplicates: vec![],
            skipped: vec![],
        };

        assert_eq!(
            format_provider_duplicate_report(&report, true),
            "merge artist 00000000-0000-0000-0000-000000000001 \"beabadoobee\" -> 00000000-0000-0000-0000-000000000002 credits=14 albums_moved=35 albums_merged=1\ndeleted: merges=1 tracks=0 albums=0 artists=0 skipped=0\n"
        );
    }
}
