from dataclasses import dataclass

from app.catalogue.application import SearchCatalogue, TrackIdentityService
from app.catalogue.domain import ProviderId
from app.persistence.in_memory.catalogue_repository import InMemoryCatalogueRepository
from app.providers.gateway import ProviderPlaybackGateway
from app.providers.youtube.catalogue import YoutubeMusicCatalogueProvider
from app.providers.youtube.playback import YouTubePlaybackResolver


@dataclass(slots=True)
class Application:
    search_catalogue: SearchCatalogue
    playback_gateway: ProviderPlaybackGateway


def build_application() -> Application:
    catalogue = InMemoryCatalogueRepository()
    identity_service = TrackIdentityService(catalogue)
    providers = (YoutubeMusicCatalogueProvider(),)

    return Application(
        search_catalogue=SearchCatalogue(
            identity_resolver=identity_service,
            providers=providers,
        ),
        playback_gateway=ProviderPlaybackGateway(
            resolvers={ProviderId.YOUTUBE_MUSIC: YouTubePlaybackResolver()}
        ),
    )
