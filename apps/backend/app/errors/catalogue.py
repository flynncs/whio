from uuid import UUID

from app.errors.base import LumenError


class TrackNotFoundError(LumenError):
    code = "track_not_found"
    public_message = "Track not found"

    def __init__(self, track_id: UUID) -> None:
        super().__init__(context={"track_id": str(track_id)})


class SourceConflictError(LumenError):
    code = "source_conflict"
    public_message = "Source is already attached to another track"
