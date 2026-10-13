import re
from urllib.parse import urlsplit, urlunsplit


THUMBNAIL_HOST_SUFFIXES = (".googleusercontent.com", ".ggpht.com")
SIZE_DIRECTIVE = re.compile(r"=(?:w\d+-h\d+|s\d+)(?:-[a-zA-Z0-9]+)*$")


def full_size_artwork_url(url: str) -> str:
    try:
        parts = urlsplit(url)
    except ValueError:
        return url
    if not (parts.hostname or "").endswith(THUMBNAIL_HOST_SUFFIXES):
        return url
    path = SIZE_DIRECTIVE.sub("=w1200-h1200", parts.path)
    if path == parts.path:
        return url
    return urlunsplit(parts._replace(path=path))
