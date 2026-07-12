from dataclasses import dataclass

from app.catalogue.application import SearchCatalogue, TrackIdentityService
from app.integrations.youtube_music.catalogue import YouTubeMusicCatalogue
from app.integrations.youtube_music.playback import YouTubeMusicPlayback
from app.persistence.in_memory.catalogue_repository import InMemoryCatalogueRepository
from app.playback.application import ResolveTrackPlayback


@dataclass(slots=True)
class Application:
    search_catalogue: SearchCatalogue
    resolve_track_playback: ResolveTrackPlayback


def build_application() -> Application:
    catalogue = InMemoryCatalogueRepository()
    identity_service = TrackIdentityService(catalogue)
    youtube_catalogue = YouTubeMusicCatalogue()
    youtube_playback = YouTubeMusicPlayback()

    return Application(
        search_catalogue=SearchCatalogue(
            identity_resolver=identity_service,
            providers=(youtube_catalogue,),
        ),
        resolve_track_playback=ResolveTrackPlayback(
            tracks=catalogue,
            playback=youtube_playback,
        ),
    )
