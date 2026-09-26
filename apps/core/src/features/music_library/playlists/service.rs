use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::features::music_library::{
    catalogue::{CatalogueError, LibraryService, SearchHit},
    domain::{
        CommentPatch, MoveTarget, OccurrenceSelector, Playlist, PlaylistEntryChange,
        PlaylistEntryError, PlaylistId, PlaylistMetadataPatch, PlaylistOccurrence, PlaylistWrite,
        PlaylistWriteOutcome, TrackId,
    },
    storage::contracts::{PlaylistStore, TrackRepositoryError},
};

#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error("playlists are not configured")]
    Disabled,

    #[error("playlist repository failed")]
    Repository(#[source] TrackRepositoryError),

    #[error("playlist entries could not be read")]
    Library(#[source] CatalogueError),

    #[error("playlist has revision {current_revision}, not the requested revision")]
    Conflict { current_revision: i64 },

    #[error("playlist entry was not applied")]
    Entry(#[source] PlaylistEntryError),
}

pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub entries: Vec<SearchHit>,
    pub editable: bool,
}

pub struct PlaylistEdit<'a> {
    pub name: Option<&'a str>,
    pub comment: Option<&'a str>,
    pub public: Option<bool>,
    pub additions: &'a [TrackId],
    pub removals: Vec<usize>,
}

pub struct PlaylistService {
    store: Option<Arc<dyn PlaylistStore>>,
    library: Arc<LibraryService>,
}

impl PlaylistService {
    pub fn new(store: Arc<dyn PlaylistStore>, library: Arc<LibraryService>) -> Self {
        Self {
            store: Some(store),
            library,
        }
    }

    pub fn disabled(library: Arc<LibraryService>) -> Self {
        Self {
            store: None,
            library,
        }
    }

    pub async fn list(&self, principal: Uuid) -> Result<Vec<Playlist>, PlaylistError> {
        self.store()?
            .list_playlists(principal)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn detail(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<Option<PlaylistDetail>, PlaylistError> {
        let store = self.store()?;
        let Some(playlist) = self.visible_playlist(principal, playlist_id).await? else {
            return Ok(None);
        };
        let editable = Self::editable_by(principal, &playlist);

        let occurrences = store
            .playlist_occurrences(playlist_id)
            .await
            .map_err(PlaylistError::Repository)?;
        let mut entries = Vec::with_capacity(occurrences.len());
        for occurrence in &occurrences {
            if let Some(hit) = self
                .library
                .track_detail(occurrence.track_id(), Some(&principal))
                .await
                .map_err(PlaylistError::Library)?
            {
                entries.push(hit);
            }
        }

        Ok(Some(PlaylistDetail {
            playlist,
            entries,
            editable,
        }))
    }

    pub async fn entries(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Vec<PlaylistOccurrence>>, PlaylistError> {
        let store = self.store()?;
        if self
            .visible_playlist(principal, playlist_id)
            .await?
            .is_none()
        {
            return Ok(None);
        }

        store
            .playlist_occurrences(playlist_id)
            .await
            .map(Some)
            .map_err(PlaylistError::Repository)
    }

    pub async fn create(
        &self,
        principal: Uuid,
        name: &str,
        comment: Option<&str>,
    ) -> Result<Playlist, PlaylistError> {
        self.store()?
            .create_playlist(principal, name, comment)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn update(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
        name: &str,
        comment: Option<&str>,
        public: bool,
    ) -> Result<Option<Playlist>, PlaylistError> {
        let write = PlaylistWrite {
            principal,
            playlist_id: playlist_id.clone(),
            expected_revision: None,
            metadata: PlaylistMetadataPatch {
                name: Some(name.to_owned()),
                comment: CommentPatch::Set(comment.map(str::to_owned)),
                public: Some(public),
            },
            entries: PlaylistEntryChange::Unchanged,
        };

        applied(self.write(write).await?)
    }

    pub async fn delete(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<bool, PlaylistError> {
        self.store()?
            .delete_playlist(principal, playlist_id)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn add_track(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, PlaylistError> {
        let write = PlaylistWrite {
            principal,
            playlist_id: playlist_id.clone(),
            expected_revision: None,
            metadata: PlaylistMetadataPatch::default(),
            entries: PlaylistEntryChange::Apply {
                removals: Vec::new(),
                additions: vec![track_id.clone()],
            },
        };

        Ok(applied(self.write(write).await?)?.is_some())
    }

    pub async fn edit(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
        edit: PlaylistEdit<'_>,
    ) -> Result<bool, PlaylistError> {
        self.edit_at_revision(principal, playlist_id, None, edit)
            .await
    }

    pub async fn edit_at_revision(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
        revision: Option<i64>,
        edit: PlaylistEdit<'_>,
    ) -> Result<bool, PlaylistError> {
        let write = PlaylistWrite {
            principal,
            playlist_id: playlist_id.clone(),
            expected_revision: revision,
            metadata: PlaylistMetadataPatch {
                name: edit.name.map(str::to_owned),
                comment: match edit.comment {
                    Some(comment) => CommentPatch::Set(Some(comment.to_owned())),
                    None => CommentPatch::Keep,
                },
                public: edit.public,
            },
            entries: PlaylistEntryChange::Apply {
                removals: edit
                    .removals
                    .iter()
                    .map(|index| {
                        OccurrenceSelector::Index(u32::try_from(*index).unwrap_or(u32::MAX))
                    })
                    .collect(),
                additions: edit.additions.to_vec(),
            },
        };

        Ok(applied(self.write(write).await?)?.is_some())
    }

    pub async fn move_entry(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
        selector: OccurrenceSelector,
        target: MoveTarget,
        revision: Option<i64>,
    ) -> Result<bool, PlaylistError> {
        let write = PlaylistWrite {
            principal,
            playlist_id: playlist_id.clone(),
            expected_revision: revision,
            metadata: PlaylistMetadataPatch::default(),
            entries: PlaylistEntryChange::Move { selector, target },
        };

        Ok(applied(self.write(write).await?)?.is_some())
    }

    pub async fn create_or_append(
        &self,
        principal: Uuid,
        playlist_id: Option<PlaylistId>,
        name: &str,
        additions: &[TrackId],
    ) -> Result<Option<PlaylistDetail>, PlaylistError> {
        let playlist_id = match playlist_id {
            Some(playlist_id) => {
                let write = PlaylistWrite {
                    principal,
                    playlist_id: playlist_id.clone(),
                    expected_revision: None,
                    metadata: PlaylistMetadataPatch {
                        name: (!name.is_empty()).then(|| name.to_owned()),
                        comment: CommentPatch::Keep,
                        public: None,
                    },
                    entries: PlaylistEntryChange::Replace(additions.to_vec()),
                };
                if applied(self.write(write).await?)?.is_none() {
                    return Ok(None);
                }

                playlist_id
            }
            None => {
                let playlist = self.create(principal, name, None).await?;
                if !additions.is_empty() {
                    let write = PlaylistWrite {
                        principal,
                        playlist_id: playlist.id().clone(),
                        expected_revision: None,
                        metadata: PlaylistMetadataPatch::default(),
                        entries: PlaylistEntryChange::Apply {
                            removals: Vec::new(),
                            additions: additions.to_vec(),
                        },
                    };
                    applied(self.write(write).await?)?;
                }

                playlist.id().clone()
            }
        };

        self.detail(principal, &playlist_id).await
    }

    fn visible_to(principal: Uuid, playlist: &Playlist) -> bool {
        playlist.owner_id() == principal || playlist.is_public()
    }

    fn editable_by(principal: Uuid, playlist: &Playlist) -> bool {
        playlist.owner_id() == principal
    }

    async fn visible_playlist(
        &self,
        principal: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>, PlaylistError> {
        let found = self
            .store()?
            .find_playlist(playlist_id)
            .await
            .map_err(PlaylistError::Repository)?;

        Ok(found.filter(|playlist| Self::visible_to(principal, playlist)))
    }

    async fn write(&self, write: PlaylistWrite) -> Result<PlaylistWriteOutcome, PlaylistError> {
        let store = self.store()?;
        let Some(current) = store
            .find_playlist(&write.playlist_id)
            .await
            .map_err(PlaylistError::Repository)?
        else {
            return Ok(PlaylistWriteOutcome::Missing);
        };
        if !Self::editable_by(write.principal, &current) {
            return Ok(PlaylistWriteOutcome::Missing);
        }

        store
            .write_playlist(write)
            .await
            .map_err(PlaylistError::Repository)
    }

    fn store(&self) -> Result<&Arc<dyn PlaylistStore>, PlaylistError> {
        self.store.as_ref().ok_or(PlaylistError::Disabled)
    }
}

fn applied(outcome: PlaylistWriteOutcome) -> Result<Option<Playlist>, PlaylistError> {
    match outcome {
        PlaylistWriteOutcome::Applied(playlist) => Ok(Some(playlist)),
        PlaylistWriteOutcome::Missing => Ok(None),
        PlaylistWriteOutcome::RevisionConflict { current_revision } => {
            Err(PlaylistError::Conflict { current_revision })
        }
        PlaylistWriteOutcome::Rejected(error) => Err(PlaylistError::Entry(error)),
    }
}
