import unittest

from app.youtube.artwork import full_size_artwork_url
from app.youtube.artist_metadata import parse_largest_thumbnail
from app.youtube.catalogue import parse_artwork_url


class ArtworkSizeTest(unittest.TestCase):
    def test_supported_hosts_replace_the_trailing_size_and_flags(self) -> None:
        for host in ["lh3.googleusercontent.com", "yt3.googleusercontent.com", "yt3.ggpht.com"]:
            for directive in ["w60-h60-l90-rj", "s60", "s544-c-k-c0x00ffffff-no-rj", "w2048-h853-p-l90-rj"]:
                with self.subTest(host=host, directive=directive):
                    self.assertEqual(
                        full_size_artwork_url(f"https://{host}/image={directive}"),
                        f"https://{host}/image=w1200-h1200",
                    )

    def test_other_hosts_and_urls_without_a_size_are_unchanged(self) -> None:
        for url in [
            "https://art.test/image=w60-h60-l90-rj",
            "https://lh3.googleusercontent.com/image",
            "https://yt3.ggpht.com/image=unknown",
            "https://lh3.googleusercontent.com.evil.test/image=s60",
            "https://googleusercontent.com/image=s60",
            "https://yt3.ggpht.com/image=s60/other",
        ]:
            self.assertEqual(full_size_artwork_url(url), url)

    def test_query_and_fragment_are_preserved(self) -> None:
        self.assertEqual(
            full_size_artwork_url("https://lh3.googleusercontent.com/image=s60?token=abc#image"),
            "https://lh3.googleusercontent.com/image=w1200-h1200?token=abc#image",
        )

    def test_artist_and_catalogue_share_the_same_rewrite(self) -> None:
        thumbnails = [{"url": "https://lh3.googleusercontent.com/image=w60-h60-l90-rj", "width": 60, "height": 60}]
        wanted = "https://lh3.googleusercontent.com/image=w1200-h1200"
        self.assertEqual(parse_artwork_url(thumbnails), wanted)
        self.assertEqual(str(parse_largest_thumbnail(thumbnails).reference), wanted)
