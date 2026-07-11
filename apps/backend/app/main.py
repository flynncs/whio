from fastapi import FastAPI

from app.api.playback import router as playback_router
from app.api.search import router as search_router

app = FastAPI()

app.include_router(search_router)
app.include_router(playback_router)


@app.get("/")
def home():
    return {"message": "Lumen is running "}


@app.get("/health")
def health():
    return {"status": "ok"}
