from fastapi import APIRouter

from app.generated.resolver_v1 import (
    CapabilitiesResponse,
    CapabilityId,
    ProviderCapabilities,
)

capability_router = APIRouter(prefix="/v1")


@capability_router.get("/capabilities", response_model=CapabilitiesResponse)
async def get_capabilities() -> CapabilitiesResponse:
    return CapabilitiesResponse(
        contract_version="1",
        providers=[
            ProviderCapabilities(
                provider_id="youtube_musc",
                capabilities=[
                    CapabilityId("catalogue.search"),
                    CapabilityId("provider.resolve"),
                ],
            )
        ],
    )
