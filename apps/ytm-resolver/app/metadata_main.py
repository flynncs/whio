from collections.abc import Mapping
from functools import partial

from fastapi import FastAPI
from fastapi.exceptions import RequestValidationError

from app.api import health
from app.api.capabilities import build_capability_router
from app.api.errors import (
    request_validation_exception_handler,
    resolver_exception_handler,
    unexpected_exception_handler,
)
from app.api.metadata import (
    AUDIODB_PROVIDER,
    METADATA_ARTIST_DISPLAY,
    ArtistMetadataFactory,
    audiodb_artist_metadata,
    build_metadata_router,
)
from app.api.request_ids import attach_request_id
from app.audiodb.client import configured_key
from app.errors import ResolverError
from app.generated.resolver_v1 import CapabilityId
from app.hosts import HostLimiter
from app.listenbrainz.artist_metadata import listenbrainz_artist_metadata
from app.listenbrainz.client import (
    LISTENBRAINZ_PROVIDER,
    configured_algorithm,
    configured_token,
)
from app.musicbrainz.artist_metadata import musicbrainz_artist_metadata
from app.musicbrainz.client import (
    MUSICBRAINZ_PROVIDER,
    configured_user_agent,
    configured_wikimedia_user_agent,
)


def configured_sources(
    limiter: HostLimiter,
) -> dict[str, ArtistMetadataFactory]:
    sources: dict[str, ArtistMetadataFactory] = {}

    key = configured_key()
    if key is not None:
        sources[AUDIODB_PROVIDER] = partial(
            audiodb_artist_metadata, key, limiter=limiter
        )

    user_agent = configured_user_agent()
    if user_agent is not None:
        sources[MUSICBRAINZ_PROVIDER] = partial(
            musicbrainz_artist_metadata,
            user_agent,
            configured_wikimedia_user_agent(),
            limiter=limiter,
        )

    token = configured_token()
    if token is not None and user_agent is not None:
        sources[LISTENBRAINZ_PROVIDER] = partial(
            listenbrainz_artist_metadata,
            token,
            user_agent,
            configured_algorithm(),
            limiter=limiter,
        )

    return sources


def capabilities_for(
    sources: Mapping[str, ArtistMetadataFactory],
) -> dict[str, tuple[CapabilityId, ...]]:
    return {
        provider_id: (METADATA_ARTIST_DISPLAY,) for provider_id in sources
    }


def build_app() -> FastAPI:
    sources = configured_sources(HostLimiter())
    capabilities = capabilities_for(sources)

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
    app.include_router(build_capability_router(capabilities, sources))
    app.include_router(
        build_metadata_router(sources=sources, capabilities=capabilities)
    )

    app.middleware("http")(attach_request_id)

    return app


app = build_app()
