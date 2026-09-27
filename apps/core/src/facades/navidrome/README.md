# the navidrome facade

this module serves the pinned Feishin client's native API (`/compat/navidrome`)
over whio's application services. the playlist region (S05) is the part with
foreign coordinates and positional item ids, so its contract lives here rather
than in code comments.

reference revision for every observation below: `navidrome/navidrome@672c0af5`;
client revision: `jeffvli/feishin@6c5173f1`.

## the move coordinates are one-based

- `server/nativeapi/playlists.go:192-195` — the move handler refuses a zero path
  coordinate (`invalid id`), so position 0 is not legal on the wire.
- `persistence/playlist_track_repository.go:281-284` and `:320` — `Reorder`
  requires `1 <= pos <= last`, clamps `newPos` to `[1, last]` and writes the
  moved row to `id = newPos`: `insert_before` is the 1-based position the entry
  **occupies** after the move.
- `persistence/playlist_track_repository.go:198-200` — `Insert`'s own doc:
  "adds tracks before the 1-based position pos".
- `core/playlists/playlists.go:192-194` — the Subsonic update converts its
  zero-based `songIndexToRemove` with `idx + 1`, so the internal position space
  is the one-based one and Subsonic is the zero-based edge.

the client settles only the destination side: `navidrome-controller.ts:1077`
sends `insert_before = endingIndex + 1`, while `:1081` sends
`trackNumber = startingIndex` unconverted, and no renderer calls
`movePlaylistItem` at the pinned revision (the reorder UI writes through
`setPlaylistSongs` → Subsonic `updatePlaylist`).

the facade converts once: `Index(trackNumber - 1)` for the source, and
`insert_before - 1` above the source / `insert_before` below it for the
destination. the second branch exists because the operation's `Before(n)` means
"insert before the occurrence that occupies pre-lift index n"
(`storage/postgres/playlists.rs:214-225`, pinned by
`storage/postgres/test_playlists.rs:766-773` and
`tests/native_playlists_http.rs:575-583`), while the reference's `insert_before`
is a destination position — a uniform `insert_before - 1` silently no-ops a
downward move.

## the react-admin window dialect

the reference is the `deluan/rest` dialect: `controller.go`'s `parseOptions`
sets `Offset = _start` and `Max = max(0, _end - _start)` — `_end` is an
**exclusive** bound — and navidrome's `persistence/sql_base_repository.go:136-137`
applies `LIMIT` only when `Max > 0`, so `_end <= _start` means **no bound** from
the offset. the wire spec (`docs/subsonic-navidrome-wire-spec.md:305`) does not
state the bound, so the reference's code is the authority.

- `GET /api/song` follows it: `max(0, _end - _start)` rows capped at
  `MAX_PAGE_LIMIT`, where that cap is also the observable form of "no bound",
  because its operation takes a bounded page.
- `GET /api/playlist/{id}/tracks` follows it without a cap: the entry list is an
  in-memory slice, so a bound past the collection returns what exists, exactly
  as the reference's `LIMIT Max` would.
- `GET /api/playlist` uses the shared `window()` + `page()` pair of the album and
  artist routes: also end-exclusive, but it falls back to the default page
  instead of to no bound when `_end <= _start`, which is observably the same
  because the cap is `MAX_PAGE_LIMIT` anyway.

## positional item ids

- an item id is the entry's 1-based position in the locked snapshot
  (`service::item_id`), emitted as the item's `id` in
  `GET /api/playlist/{id}/tracks` and in a playlist's inline `songs`, with the
  canonical track id in `mediaFileId` and the playlist id in `playlistId`.
- it is resolved inside the write as `OccurrenceSelector::Index(item_id - 1)`,
  never against a facade-side read.
- it **may change on reorder**; the native `entry_id` does not. zero and
  past-the-end ids are client errors (422 and 404).
- `POST /api/playlist/{id}/tracks` adds and `DELETE .../tracks?id=` removes by
  those ids, so repeated tracks are distinct occurrences and removing one
  positional id leaves the others.

## last-write-wins, and the stale-position limitation

the compat protocol has no revision field, so every facade write passes
`expected_revision: None`: a concurrent edit is not detected and the last write
wins. an item id a client computed before another edit landed may address a
different occurrence, and this wire cannot detect that. the limitation is
documented here rather than claimed away; a stale positional id is still refused
(404) instead of silently editing a different entry.

## deliberate divergences from the reference

| verb | reference | this facade | why |
| --- | --- | --- | --- |
| `POST /api/playlist` | always creates (`persistence/playlist_repository.go:404-412` forces `pls.ID = ""`) | an optional body `id` names an existing playlist to replace, preserving name, comment and visibility | the S05 packet's create contract (S01 C2) |
| `GET /api/playlist/{id}` | returns the playlist's tracks (`server/nativeapi/playlists.go:57-66`) | returns the playlist record the pinned client reads (`navidrome-controller.ts:683-693`) | pre-existing shape; the items have their own route |
| `DELETE .../tracks?id=` | a stale item id deletes nothing and returns 200 | 404 | a stale positional id is a client error, not a silent wrong edit |
| list `_sort` (other than `name`), `_order=DESC`, `q`, `owner_id`, `smart=true`, `rules`, `sync=true` | honoured | 422 | unsupported selectors are refused, not silently ignored |
| `ownerId` on create/update | the react-admin adapter would apply it | 403 unless it names the caller | ordinary clients cannot select another owner (S01 §5) |

## fields and errors

the playlist DTO carries `id, name, comment, ownerId, ownerName, public,
songCount, duration, createdAt, updatedAt` and `songs` only where the entries
were read. the client also reads `dominantColor`, `evaluatedAt`, `rules`, `size`,
`sync`, `thumbHash`, `uploadedImage` and `path`; no operation sources them, so
they are absent rather than defaulted. `updatedAt` **is** sourced and emitted.

errors are `403` for a non-owner editing a visible playlist, `404` for an
unknown or invisible playlist and for an unresolvable positional id, `422` for a
malformed coordinate, an invalid name or comment, or an unsupported
selector/owner, and `500` for a repository failure.

## residual gaps

1. the list route reads each playlist's detail to fill `songs` (an N+1), kept
   because `tests/compat_http.rs navidrome_playlist_surface_lists_its_songs`
   pins `songs` on the list payload.
2. an update or add with no present fields still commits (the revision and
   `updatedAt` advance); the reference returns early.
3. the playlist list has no search, no sort beyond name-ascending and no
   operation-side paging; those inputs are refused and the window is applied in
   memory.
4. the image verbs (`POST`/`DELETE /api/playlist/{id}/image`) are not
   registered.
