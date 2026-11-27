# Media: ffmpeg as a component, media.probe

Phase 1 of the media plan. Each part is usable without the others:

1. **ffmpeg as a setup component** (`media.components`, `media.install`): nothing is downloaded unless the owner says so.
2. **`media.probe`**: what a media file holds.

Every part says what is missing and how to get it instead of failing in the middle: without ffmpeg a feature that
needs it answers `501` with the sentence "ffmpeg is not installed: `media.install` with confirm: true installs the
pinned build ...".

## The ffmpeg component

`ffmpeg` and `ffprobe` run as separate processes (the hub links no media library). They are found in this order:

| Where | Used |
|---|---|
| the `ffmpeg_dir` setting, else `MM_FFMPEG_DIR` | as it is: the folder the owner points to |
| `<home>/engines/ffmpeg/` | after its files are hashed against `install.json` (see below) |
| `PATH` | the first folder that holds both programs |

`media.components` (also `GET /v1/media/components`) reports each component: where ffmpeg is, its version and licence
(`LGPL`, `GPL` or `nonfree`, read from the build's `configuration:` line; a GPL build is reported, not refused: what runs
on the owner's machine is the owner's call), the pinned build and the speech component (`built`, the models found).

### `media.install`

```bash
model-mistress call media.install '{"confirm": false, "url": "https://.../ffmpeg-n9.0-win64-lgpl.zip", "sha256": "<64 hex>", "size": 164000000}'
```

- Without `confirm: true` the answer (a `400`) says what would happen: the file, its size, where it comes from and the
  SHA-256 it must have. Nothing is fetched.
- **No build is pinned in this release.** A pin needs a file name, an address that never changes, a size and a SHA-256 that
  someone has downloaded and hashed, and the owner chooses the build (an LGPL one: no x264 or x265). Until then give
  `url` and `sha256` (an `https` address, or one on this machine), or `file` and `sha256` for an archive you downloaded
  yourself, or set `ffmpeg_dir`. `pin()` in `src/media/ffmpeg.rs` is the one place to fill in when the owner has chosen.
- The archive's hash is checked before anything is unpacked; a mismatch (`422 hash_mismatch` as a job error) installs
  nothing. The unpacked programs are run once (`-version`) before they replace anything.
- The result goes to `<home>/engines/ffmpeg/` (the player, `ffplay`, is left out). An earlier install is kept beside it as
  `ffmpeg.old-<time>`, never deleted. `install.json` records the size and SHA-256 of every installed file; they are
  checked again the first time a file is used and whenever its size or time changes. A file that was swapped, or a folder
  with no record, is an **error that says how to put it right** (`media.install` with `force: true`), never a silent use.
- It runs as a job (`job.get` shows the download); `wait: true` answers when it has finished. Running it again with the
  same archive says `already installed` unless `force: true`.

## `media.probe`

`media.probe` (`POST /v1/call/media.probe {"file": ...}`, and `POST /v1/media/probe`) runs ffprobe and answers the
container (format, duration, size, bit rate), every video stream (codec, size, frame rate, frames, rotation, cover art),
audio stream (codec, sample rate, channels, language) and subtitle stream, and `has_video` / `has_audio`.
`POST /v1/media/probe` takes either JSON `{"file": "<path on the hub's machine>"}` or a multipart upload with a `file`
field (up to 512 MB; kept in `<home>/media/tmp` while ffprobe reads it, then removed). Needs ffmpeg.

## Not run, and open

See the changelog entry for what was run and what was not.
