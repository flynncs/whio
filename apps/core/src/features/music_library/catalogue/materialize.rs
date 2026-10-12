use std::{collections::HashSet, sync::Arc};

use crate::features::music_library::{
    domain::{
        Album, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtworkRef, SourceIdentity,
        SourceKind, TrackHit,
    },
    storage::contracts::{CatalogueWrite, SourceLookup, TrackRepositoryError},
};

use super::CatalogueError;
use super::domain::CatalogueCandidate;

pub(crate) async fn materialize_artist(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    credit: &ArtistCredit,
) -> Result<Artist, CatalogueError> {
    if let Some(source) = credit.provider()
        && let Some(artist) = sources
            .find_artist_by_source(source)
            .await
            .map_err(CatalogueError::Repository)?
    {
        return Ok(artist);
    }

    let artist = catalogue
        .resolve_or_create_artist(credit)
        .await
        .map_err(CatalogueError::Repository)?;
    if let Some(source) = credit.provider() {
        catalogue
            .set_artist_source(artist.id(), source)
            .await
            .map_err(CatalogueError::Repository)?;
    }
    Ok(artist)
}

pub(crate) async fn materialize_album(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    metadata: &AlbumMetadata,
) -> Result<Album, CatalogueError> {
    if let Some(source) = metadata.provider()
        && let Some(album) = sources
            .find_album_by_source(source)
            .await
            .map_err(CatalogueError::Repository)?
    {
        store_album_artwork(sources, source, metadata.artwork()).await;
        return Ok(album);
    }

    if let Some(artist) = metadata.album_artist() {
        materialize_artist(catalogue, sources, artist).await?;
    }
    let album = catalogue
        .resolve_or_create_album(metadata)
        .await
        .map_err(CatalogueError::Repository)?;
    if let Some(source) = metadata.provider() {
        catalogue
            .set_album_source(album.id(), source)
            .await
            .map_err(CatalogueError::Repository)?;
        store_album_artwork(sources, source, metadata.artwork()).await;
    }
    Ok(album)
}

async fn store_album_artwork(
    sources: &Arc<dyn SourceLookup>,
    source: &SourceIdentity,
    artwork: Option<&ArtworkRef>,
) {
    let Some(artwork) = artwork else {
        return;
    };
    if let Err(error) = sources.set_album_source_artwork(source, artwork).await {
        tracing::warn!(%error, "album artwork could not be stored");
    }
}

pub(crate) async fn materialize(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    candidates: Vec<CatalogueCandidate>,
) -> Result<Vec<TrackHit>, CatalogueError> {
    materialize_candidates(catalogue, sources, candidates, false).await
}

pub(crate) async fn materialize_album_tracks(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    candidates: Vec<CatalogueCandidate>,
) -> Result<Vec<TrackHit>, CatalogueError> {
    materialize_candidates(catalogue, sources, candidates, true).await
}

async fn materialize_candidates(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    candidates: Vec<CatalogueCandidate>,
    placed: bool,
) -> Result<Vec<TrackHit>, CatalogueError> {
    let mut hits = Vec::with_capacity(candidates.len());
    let mut memberships = HashSet::new();
    for (position, candidate) in candidates.into_iter().enumerate() {
        let position = u32::try_from(position)
            .map_err(|_| CatalogueError::Repository(TrackRepositoryError::InvalidData))?;
        let membership = if placed {
            candidate.album_track(position)
        } else {
            AlbumTrack::unplaced()
        };
        let source = candidate.source().clone();
        let artwork = candidate.artwork().cloned();
        let album_metadata = candidate.album().cloned();
        let metadata = candidate.metadata().clone();
        let credits = metadata.artists().to_vec();
        let preserve_membership_for_match =
            metadata.musicbrainz_recording_id().is_none() && metadata.isrc().is_none();

        let album = match &album_metadata {
            Some(metadata) => Some(materialize_album(catalogue, sources, metadata).await?),
            None => None,
        };
        let track = catalogue
            .get_or_create_provider(source.clone(), metadata, album.as_ref().map(Album::id))
            .await
            .map_err(CatalogueError::Repository)?;
        let has_library_source = sources
            .find_sources(track.id())
            .await
            .map_err(CatalogueError::Repository)?
            .iter()
            .any(|source| SourceKind::of(source.provider_id()).is_library());
        let preserve_membership = preserve_membership_for_match && has_library_source;

        let album = match album {
            Some(album) => {
                if memberships.insert((album.id().clone(), track.id().clone())) {
                    if !placed || preserve_membership {
                        catalogue
                            .add_album_track(album.id(), track.id(), &membership)
                            .await
                            .map_err(CatalogueError::Repository)?;
                    } else {
                        catalogue
                            .set_album_track(album.id(), track.id(), &membership)
                            .await
                            .map_err(CatalogueError::Repository)?;
                    }
                }

                Some(album)
            }
            None => None,
        };

        for credit in &credits {
            if credit.provider().is_some() {
                materialize_artist(catalogue, sources, credit).await?;
            }
        }

        if let Some(artwork) = artwork
            && let Err(error) = sources.set_source_artwork(&source, &artwork).await
        {
            tracing::warn!(%error, "artwork could not be stored");
        }

        hits.push(TrackHit::new(track, album, vec![source]));
    }

    Ok(hits)
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "materialize_tests.rs"]
mod tests;
