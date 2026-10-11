use std::sync::Arc;

use crate::features::music_library::{
    domain::{Album, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, SourceKind, TrackHit},
    storage::contracts::{CatalogueWrite, SourceLookup},
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
    }
    Ok(album)
}

pub(crate) async fn materialize(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    candidates: Vec<CatalogueCandidate>,
) -> Result<Vec<TrackHit>, CatalogueError> {
    let mut hits = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let source = candidate.source().clone();
        let artwork = candidate.artwork().cloned();
        let album_metadata = candidate.album().cloned();
        let metadata = candidate.metadata().clone();
        let credits = metadata.artists().to_vec();
        let preserve_membership_for_match =
            metadata.musicbrainz_recording_id().is_none() && metadata.isrc().is_none();

        let track = catalogue
            .get_or_create_provider(source.clone(), metadata)
            .await
            .map_err(CatalogueError::Repository)?;
        let has_library_source = sources
            .find_sources(track.id())
            .await
            .map_err(CatalogueError::Repository)?
            .iter()
            .any(|source| SourceKind::of(source.provider_id()).is_library());
        let preserve_membership = preserve_membership_for_match && has_library_source;

        let album = match &album_metadata {
            Some(album_metadata) => {
                let album = materialize_album(catalogue, sources, album_metadata).await?;
                if !preserve_membership {
                    catalogue
                        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                        .await
                        .map_err(CatalogueError::Repository)?;
                }

                Some(album)
            }
            None => {
                if !preserve_membership {
                    catalogue
                        .clear_album_track(track.id())
                        .await
                        .map_err(CatalogueError::Repository)?;
                }

                None
            }
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
