from uuid import UUID

from app.errors.base import LumenError


class RecordingNotFoundError(LumenError):
    code = "recording_not_found"
    public_message = "Recording not found"

    def __init__(self, recording_id: UUID) -> None:
        super().__init__(context={"recording_id": str(recording_id)})


class SourceConflictError(LumenError):
    code = "source_conflict"
    public_message = "Source is already attached to another recording"
