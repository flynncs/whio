from typing import Annotated

from fastapi import Depends, Request

from app.bootstrap.application import Application
from app.catalogue.application import SearchCatalogue
from app.providers.gateway import ProviderPlaybackGateway


def get_application(request: Request) -> Application:
    application = getattr(request.app.state, "application", None)

    if not isinstance(application, Application):
        raise RuntimeError("Application is not configured")

    return application


def get_search_catalogue(
    application: Annotated[Application, Depends(get_application)],
) -> SearchCatalogue:
    return application.search_catalogue


def get_playback_gateway(
    application: Annotated[Application, Depends(get_application)],
) -> ProviderPlaybackGateway:
    return application.playback_gateway
