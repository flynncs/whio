import unittest

from app.catalogue.domain import ProviderId


class ProviderIdTests(unittest.TestCase):
    def test_provider_name_uses_stable_wire_values(self) -> None:
        self.assertEqual(ProviderId.YOUTUBE_MUSIC.value, "youtube_music")
        self.assertEqual(ProviderId.NAVIDROME.value, "navidrome")

    def test_unknown_provider_name_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            ProviderId("not_a_supported_provider")


if __name__ == "__main__":
    unittest.main()
