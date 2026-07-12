from dataclasses import dataclass

from app.domain.catalogue_repository import InMemoryCatalogueRepository
from app.integrations.youtube.catalogue import YoutubeMusicCatalogueProvider
from app.integrations.youtube.playback.resolver import YouTubePlaybackResolver
from app.playback.service import PlaybackService
from app.services.search import SearchService


@dataclass(slots=True)
class Application:
    search_service: SearchService
    playback_service: PlaybackService


def create_application() -> Application:
    catalogue = InMemoryCatalogueRepository()
    providers = (YoutubeMusicCatalogueProvider(),)

    return Application(
        search_service=SearchService(repository=catalogue, providers=providers),
        playback_service=PlaybackService(
            resolvers={"youtube_music": YouTubePlaybackResolver()}
        ),
    )
