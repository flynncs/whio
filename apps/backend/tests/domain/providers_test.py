import unittest

from app.domain.providers import ProviderName


class ProviderNameTests(unittest.TestCase):
    def test_provider_name_uses_stable_wire_values(self) -> None:
        self.assertEqual(ProviderName.YOUTUBE_MUSIC.value, "youtube_music")
        self.assertEqual(ProviderName.NAVIDROME.value, "navidrome")

    def test_unknown_provider_name_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            ProviderName("not_a_supported_provider")


if __name__ == "__main__":
    unittest.main()
