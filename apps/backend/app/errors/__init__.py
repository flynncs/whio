from app.errors.base import LumenError
from app.errors.catalogue import SourceConflictError, TrackNotFoundError
from app.errors.playback import NoPlayableSourceError, PlaybackProviderMismatchError
from app.errors.search import ProviderUnavailableError, SearchUnavailableError

__all__ = [
    "LumenError",
    "ProviderUnavailableError",
    "TrackNotFoundError",
    "PlaybackProviderMismatchError",
    "NoPlayableSourceError",
    "SearchUnavailableError",
    "SourceConflictError",
]
