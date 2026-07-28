from fastapi import FastAPI

from app.api import capabilities, catalogue, health, playback
from app.api.request_ids import attach_request_id

app = FastAPI()

app.include_router(health.health_router)
app.include_router(capabilities.capability_router)
app.include_router(catalogue.catalogue_router)
app.include_router(playback.playback_router)
app.middleware("http")(attach_request_id)
