from uuid import uuid7

from fastapi import FastAPI, Request
from starlette.responses import Response

from app.api.errors import lumen_error_handler
from app.api.playback import router as playback_router
from app.api.search import router as search_router
from app.errors import LumenError

app = FastAPI()

app.add_exception_handler(LumenError, lumen_error_handler)


@app.middleware("http")
async def add_request_id(request: Request, call_next) -> Response:
    request_id = request.headers.get("X-Request-ID") or str(uuid7())
    request.state.request_id = request_id

    response = await call_next(request)
    response.headers["X-Request-ID"] = request_id
    return response


app.include_router(search_router)
app.include_router(playback_router)


@app.get("/")
def home():
    return {"message": "Lumen is running "}


@app.get("/health")
def health():
    return {"status": "ok"}
