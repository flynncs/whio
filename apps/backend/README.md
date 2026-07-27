# existing python backend

this is the original FastAPI proof of concept.

it currently proves:

- youtube music search
- yt-dlp playback resolution
- proxy streaming with range support
- the basic lumen track/source split

the core is moving to rust. don't delete this app yet: it is the behaviour reference for the rust vertical slice, and its youtube-specific code will become the first optional resolver service.

new core domain work should go into the rust app once that exists. changes here should mainly keep the poc working or help extract the youtube resolver cleanly.
