from collections.abc import Mapping, Sequence

from fastapi import APIRouter

from app.api.metadata import (
    ArtistMetadataFactory,
    validate_metadata_dispatch,
)
from app.errors import ProviderUnavailableError
from app.generated.resolver_v1 import (
    CapabilitiesResponse,
    CapabilityId,
    ProviderCapabilities,
)

CONTRACT_VERSION = "1"


def build_capability_router(
    capabilities: Mapping[str, Sequence[CapabilityId]],
    handlers: Mapping[str, ArtistMetadataFactory],
) -> APIRouter:
    advertised = {
        provider_id: tuple(provider_capabilities)
        for provider_id, provider_capabilities in capabilities.items()
    }
    validate_advertised_capabilities(advertised, handlers)

    router = APIRouter(prefix="/v1")

    @router.get("/capabilities", response_model=CapabilitiesResponse)
    async def get_capabilities() -> CapabilitiesResponse:
        if not advertised:
            raise ProviderUnavailableError
        return capabilities_response(advertised)

    return router


def capabilities_response(
    capabilities: Mapping[str, Sequence[CapabilityId]],
) -> CapabilitiesResponse:
    return CapabilitiesResponse(
        contract_version=CONTRACT_VERSION,
        providers=[
            ProviderCapabilities(
                provider_id=provider_id,
                capabilities=list(provider_capabilities),
            )
            for provider_id, provider_capabilities in capabilities.items()
        ],
    )


def validate_advertised_capabilities(
    capabilities: Mapping[str, Sequence[CapabilityId]],
    handlers: Mapping[str, ArtistMetadataFactory],
) -> None:
    validate_metadata_dispatch(handlers, capabilities)
