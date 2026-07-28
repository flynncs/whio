from fastapi import APIRouter

from app.generated.resolver_v1 import HealthResponse, ReadyResponse

health_router = APIRouter(prefix="/health")


@health_router.get("/live", response_model=HealthResponse)
async def get_liveness() -> HealthResponse:
    return HealthResponse(status="ok")


@health_router.get("/ready", response_model=ReadyResponse)
async def get_readiness() -> ReadyResponse:
    return ReadyResponse(status="ready")
