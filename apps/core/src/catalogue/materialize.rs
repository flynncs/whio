use std::sync::Arc;

use crate::tracks::{AlbumTrack, CatalogueWrite, SourceLookup, TrackHit};

use super::CatalogueError;
use super::domain::CatalogueCandidate;

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

        let track = catalogue
            .get_or_create(source.clone(), metadata)
            .await
            .map_err(CatalogueError::Repository)?;

        let album = match &album_metadata {
            Some(album_metadata) => {
                let album = catalogue
                    .resolve_or_create_album(album_metadata)
                    .await
                    .map_err(CatalogueError::Repository)?;
                if let Some(provider) = album_metadata.provider() {
                    catalogue
                        .set_album_source(album.id(), provider)
                        .await
                        .map_err(CatalogueError::Repository)?;
                }
                catalogue
                    .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                    .await
                    .map_err(CatalogueError::Repository)?;

                Some(album)
            }
            None => {
                catalogue
                    .clear_album_track(track.id())
                    .await
                    .map_err(CatalogueError::Repository)?;

                None
            }
        };

        for credit in &credits {
            if let Some(provider) = credit.provider() {
                let artist = catalogue
                    .resolve_or_create_artist(credit)
                    .await
                    .map_err(CatalogueError::Repository)?;
                catalogue
                    .set_artist_source(artist.id(), provider)
                    .await
                    .map_err(CatalogueError::Repository)?;
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
