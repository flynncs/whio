from dataclasses import dataclass

from app.domain.catalogue_repository import InMemoryCatalogueRepository
from app.domain.providers import ProviderName
from app.integrations.youtube.catalogue import YoutubeMusicCatalogueProvider
from app.integrations.youtube.playback.resolver import YouTubePlaybackResolver
from app.playback.service import PlaybackService
from app.services.search import SearchService
from app.services.track_identity import TrackIdentityService


@dataclass(slots=True)
class Application:
    search_service: SearchService
    playback_service: PlaybackService


def create_application() -> Application:
    catalogue = InMemoryCatalogueRepository()
    identity_service = TrackIdentityService(catalogue)
    providers = (YoutubeMusicCatalogueProvider(),)

    return Application(
        search_service=SearchService(
            identity_resolver=identity_service, providers=providers
        ),
        playback_service=PlaybackService(
            resolvers={ProviderName.YOUTUBE_MUSIC: YouTubePlaybackResolver()}
        ),
    )
