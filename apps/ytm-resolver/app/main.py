from fastapi import FastAPI

from app.api import capabilities, health

app = FastAPI()

app.include_router(health.health_router)
app.include_router(capabilities.capability_router)
