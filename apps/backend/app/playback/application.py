from uuid import UUID

from app.catalogue.ports import TrackRepository
from app.errors.catalogue import TrackNotFoundError
from app.errors.playback import NoPlayableSourceError
from app.playback.domain import ResolvedPlaybackSource
from app.playback.ports import PlaybackGateway


class ResolveTrackPlayback:
    def __init__(
        self,
        tracks: TrackRepository,
        playback: PlaybackGateway,
    ) -> None:
        self._tracks = tracks
        self._playback = playback

    async def execute(self, track_id: UUID) -> ResolvedPlaybackSource:
        track = self._tracks.get_track(track_id)

        if track is None:
            raise TrackNotFoundError(track_id)

        sources = self._tracks.get_sources(track_id)
        if not sources:
            raise NoPlayableSourceError()

        # Todo: proper resolution
        return await self._playback.resolve(sources[0])
