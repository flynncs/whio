import re
from urllib.parse import urlsplit, urlunsplit


THUMBNAIL_HOSTS = {"lh3.googleusercontent.com", "yt3.ggpht.com"}
SIZE_DIRECTIVE = re.compile(r"=(?:w\d+-h\d+|s\d+)(?:-[a-zA-Z0-9]+)*$")


def full_size_artwork_url(url: str) -> str:
    try:
        parts = urlsplit(url)
    except ValueError:
        return url
    if parts.hostname not in THUMBNAIL_HOSTS:
        return url
    path = SIZE_DIRECTIVE.sub("=w1200-h1200", parts.path)
    if path == parts.path:
        return url
    return urlunsplit(parts._replace(path=path))
