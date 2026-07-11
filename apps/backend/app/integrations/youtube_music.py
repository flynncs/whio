from ytmusicapi import YTMusic

ytmusic = YTMusic()


def search_songs(query: str) -> list[dict]:
    results = ytmusic.search(query, filter="songs", limit=5)

    return [
        {
            "video_id": result["videoId"],
            "title": result["title"],
            "artists": [artist["name"] for artist in result.get("artists", [])],
            "duration": result.get("duration"),
        }
        for result in results
    ]
