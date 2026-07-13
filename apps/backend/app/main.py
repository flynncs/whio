from uuid import uuid7

from fastapi import FastAPI, Request
from starlette.responses import Response

from app.bootstrap.application import build_application
from app.delivery.http.catalogue_routes import router as catalogue_router
from app.delivery.http.errors import (
    lumen_error_handler,
    unexpected_error_handler,
)
from app.delivery.http.playback_routes import router as playback_router
from app.errors.base import LumenError

app = FastAPI()
app.state.application = build_application()
app.add_exception_handler(LumenError, lumen_error_handler)
app.add_exception_handler(Exception, unexpected_error_handler)


@app.middleware("http")
async def add_request_id(request: Request, call_next) -> Response:
    request_id = request.headers.get("X-Request-ID") or str(uuid7())
    request.state.request_id = request_id

    response = await call_next(request)
    response.headers["X-Request-ID"] = request_id
    return response


app.include_router(catalogue_router)
app.include_router(playback_router)


@app.get("/")
def home():
    return {"message": "Lumen is running "}


@app.get("/health")
def health():
    return {"status": "ok"}
