class ResolverError(Exception):
    pass


class UnsupportedProviderError(ResolverError):
    pass


class PlaybackResolutionError(ResolverError):
    pass


class ProviderUnavailableError(ResolverError):
    pass
