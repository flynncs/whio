import unittest

from pydantic import ValidationError

from app.api.schemas.search import SearchQuery, SearchResponse, SearchResult


class SearchSchemaTests(unittest.TestCase):
    def test_search_query_uses_default_limit(self) -> None:
        query = SearchQuery(query="Daft Punk")

        self.assertEqual(query.query, "Daft Punk")
        self.assertEqual(query.limit, 5)

    def test_search_query_accepts_valid_limit(self) -> None:
        query = SearchQuery(query="Daft Punk", limit=10)

        self.assertEqual(query.limit, 10)

    def test_search_query_rejects_empty_query(self) -> None:
        with self.assertRaises(ValidationError):
            SearchQuery(query="")

    def test_search_query_rejects_out_of_range_limit(self) -> None:
        with self.assertRaises(ValidationError):
            SearchQuery(query="Daft Punk", limit=0)

        with self.assertRaises(ValidationError):
            SearchQuery(query="Daft Punk", limit=26)

    def test_search_query_rejects_unknown_fields(self) -> None:
        with self.assertRaises(ValidationError):
            SearchQuery(query="Daft Punk", unexpected="value")

    def test_search_response_validates_nested_results(self) -> None:
        response = SearchResponse(
            results=[
                SearchResult(
                    provider="youtube_music",
                    external_id="abc123",
                    title="Instant Crush",
                    artists=["Daft Punk"],
                    duration_seconds=337,
                )
            ]
        )

        self.assertEqual(response.results[0].title, "Instant Crush")


if __name__ == "__main__":
    unittest.main()
