import json
import subprocess


def resolve_audio(video_id: str) -> dict:
    command = [
        "yt-dlp",
        "--ignore-config",
        "--no-playlist",
        "--format",
        "bestaudio[acodec=opus]/bestaudio",
        "--dump-single-json",
        f"https://www.youtube.com/watch?v={video_id}",
    ]

    result = subprocess.run(
        command, capture_output=True, text=True, check=True, timeout=30
    )

    stream = json.loads(result.stdout)

    return {
        "url": stream["url"],
        "headers": stream.get("http_headers", {}),
        "codec": stream.get("acodec"),
        "bitrate": stream.get("abr"),
        "duration": stream.get("duration"),
    }
