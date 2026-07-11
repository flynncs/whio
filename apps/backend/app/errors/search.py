from app.errors.base import LumenError


class ProviderUnavailableError(LumenError):
    code = "provider_unavailable"
    public_message = "Catalogue provider is unavailable"


class SearchUnavailableError(LumenError):
    code = "search_unavailable"
    public_message = "Search is temporarily unavailable"
