from fastapi import FastAPI

from app.api import capabilities, catalogue, health, playback
from app.api.errors import resolver_exception_handler
from app.api.request_ids import attach_request_id
from app.errors import ResolverError

app = FastAPI()

app.add_exception_handler(ResolverError, resolver_exception_handler)

app.include_router(health.health_router)
app.include_router(capabilities.capability_router)
app.include_router(catalogue.catalogue_router)
app.include_router(playback.playback_router)

app.middleware("http")(attach_request_id)
