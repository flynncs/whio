from http import HTTPStatus

from app.errors.base import LumenError


class ProviderUnavailableError(LumenError):
    code = "provider_unavailable"
    public_message = "Catalogue provider is unavailable"
    status_code = HTTPStatus.SERVICE_UNAVAILABLE


class SearchUnavailableError(LumenError):
    code = "search_unavailable"
    public_message = "Search is temporarily unavailable"
    status_code = HTTPStatus.SERVICE_UNAVAILABLE
