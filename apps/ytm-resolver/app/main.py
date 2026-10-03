from collections.abc import Mapping

from fastapi import FastAPI
from fastapi.exceptions import RequestValidationError

from app.api import catalogue, health, playback
from app.api.capabilities import build_capability_router
from app.api.errors import (
    request_validation_exception_handler,
    resolver_exception_handler,
    unexpected_exception_handler,
)
from app.api.metadata import (
    METADATA_ARTIST_DISPLAY,
    YOUTUBE_MUSIC_PROVIDER,
    ArtistMetadataFactory,
    build_metadata_router,
    youtube_music_artist_metadata,
)
from app.api.request_ids import attach_request_id
from app.errors import ResolverError
from app.generated.resolver_v1 import CapabilityId

YOUTUBE_MUSIC_HANDLERS: Mapping[str, ArtistMetadataFactory] = {
    YOUTUBE_MUSIC_PROVIDER: youtube_music_artist_metadata,
}

YOUTUBE_MUSIC_CAPABILITIES: Mapping[str, tuple[CapabilityId, ...]] = {
    YOUTUBE_MUSIC_PROVIDER: (
        CapabilityId("catalogue.search"),
        CapabilityId("catalogue.album"),
        CapabilityId("playback.resolve"),
        METADATA_ARTIST_DISPLAY,
    )
}


def build_app() -> FastAPI:
    app = FastAPI()

    app.add_exception_handler(ResolverError, resolver_exception_handler)
    app.add_exception_handler(
        RequestValidationError,
        request_validation_exception_handler,
    )
    app.add_exception_handler(
        Exception,
        unexpected_exception_handler,
    )

    app.include_router(health.health_router)
    app.include_router(
        build_capability_router(YOUTUBE_MUSIC_CAPABILITIES, YOUTUBE_MUSIC_HANDLERS)
    )
    app.include_router(catalogue.catalogue_router)
    app.include_router(
        build_metadata_router(
            sources=YOUTUBE_MUSIC_HANDLERS,
            capabilities=YOUTUBE_MUSIC_CAPABILITIES,
        )
    )
    app.include_router(playback.playback_router)

    app.middleware("http")(attach_request_id)

    return app


app = build_app()
