# Hearthsound Lidarr acquisition plugin

A [Hearthsound](https://github.com/hearthsound) acquisition plugin that asks a
[Lidarr](https://lidarr.audio) instance to find and download a whole album. It
compiles to a WebAssembly component implementing the `acquisition` provider
interface at album granularity: the server hands the plugin an album and its
wanted tracks, the plugin looks the album up, makes sure Lidarr holds the
album's artist, monitors the album and starts Lidarr's search, and reports the
files Lidarr imported once every wanted track is present in the configured
import root.

The plugin does not download anything itself. Lidarr owns the indexers, the
download client and the import into its root folder; the plugin reads Lidarr's
API and hands the imported files to the server.

The plugin never deletes, modifies or retags a file Lidarr created, and it never
cancels Lidarr's queue. Cancelling an acquisition only forgets the plugin's own
state.

## Requirements

- Rust 1.99 or newer, with the `wasm32-wasip2` target. The included
  `rust-toolchain.toml` pins both; `rustup toolchain install` fetches them.
- A Lidarr instance reachable over HTTP from the server that hosts the plugin,
  with its API key.
- An import root that exposes the files of Lidarr's root folder, named by id.
  **The import root must use mode `link`**: the server then imports Lidarr's
  files by hardlink (or by copy when a hardlink is impossible) and leaves
  Lidarr's originals byte-for-byte unchanged.
- Lidarr's root folder path, exactly as Lidarr reports it, so the plugin can
  turn a track file path into a path inside the import root.

## Build

```console
cargo build --release --locked --target wasm32-wasip2
```

The component lands at
`target/wasm32-wasip2/release/hearthsound_plugin_lidarr.wasm`.

## Check and pack

Install the plugin CLI from the SDK repository:

```console
cargo install --locked --git https://github.com/hearthsound/plugin-sdk --tag v0.3.0 hearthsound-plugin-cli
```

Then validate the manifest and the component:

```console
hsplugin check
```

It prints `ok: app.hearthsound.lidarr 0.1.1 (provides acquisition)` on success.

Finally, build the archive you install on a server:

```console
hsplugin pack --out-dir dist
```

`dist/app.hearthsound.lidarr-0.1.1.hsplugin` contains the manifest and the
component.

## Tests

The lookup mapping, the quality cutoff mapping, the download state machine, the
track file matching and the clock helpers run on the host target, so their unit
tests need no WebAssembly toolchain and no Lidarr instance:

```console
cargo test
```

The download tests drive the real sequence - ensure the artist, monitor the
album, start the search, poll the queue and the history, check the files -
against a recorded Lidarr facade, so they assert behavior rather than wiring.

## Configuration

| Instance setting | Default | Meaning |
|---|---|---|
| `url` | `http://lidarr:8686` | Base URL of the Lidarr instance. |
| `apiKey` | - | Lidarr API key, sent as `X-Api-Key`. |
| `importRoot` | - | Id of the import root whose path holds Lidarr's root folder files. Must use mode `link`. |
| `rootFolderPath` | - | Lidarr's root folder path, as Lidarr sees it, for example `/music`. |
| `qualityProfileId` | `1` | Quality profile a newly added artist is stored with. |
| `metadataProfileId` | `1` | Metadata profile a newly added artist is stored with. |
| `searchWaitMinutes` | `30` | How long Lidarr may take to move the album into its queue before the acquisition fails. |

`validate-config` checks the document and then asks the instance for its system
status, so a wrong URL or API key is reported when the configuration is saved.

## Notes

- An album lookup uses `lidarr:<release-group id>` when the query carries an
  `mb-release-group` id, and `<album artist> <album>` otherwise. Lidarr resolves the
  first exactly, so it does not depend on how closely the text matches.
- Lidarr's text search matches on any word, so it can return unrelated albums
  that share one. Only results whose artist and title name the requested album
  become candidates, compared without case, accents, punctuation or a trailing
  edition qualifier such as `(Deluxe Edition)`; an album whose title differs
  otherwise is never offered, so its artist is never added.
- A candidate lists one file per wanted track that also appears in the release
  tracklist Lidarr sent, named `<NN> - <title>.<ext>`, so a standard edition
  offered for a deluxe request lists only the tracks it holds and the server
  decides whether that is enough. Lidarr omits tracklists from most lookup
  results; then the candidate lists every wanted track. The format follows the
  quality profile's cutoff (`FLAC`, `MP3-320`, `AAC-256`, and so on); a cutoff
  the plugin does not recognize is reported as FLAC.
- When Lidarr does not hold the album's artist yet, the plugin adds it with
  `addOptions.monitor` `none`, no automatic search and the one album named in
  `albumsToMonitor`. It then monitors that album and posts an `AlbumSearch`
  command. An artist Lidarr already holds is never modified.
- Lidarr adds a new artist asynchronously, so the album may not be in its
  library on the first call. The plugin looks again on every poll until the
  album appears, and fails only after `searchWaitMinutes`.
- A download follows Lidarr's own album state. Once Lidarr lists the album, the
  plugin reads its tracks and track files: it completes when every album track
  that maps to a listed wanted track has a file inside `rootFolderPath` that is
  a non-empty regular file in the import root. A wanted track Lidarr's album
  does not hold is not waited for; the server fans it out as a track
  acquisition. An album Lidarr reports fully imported with nothing in its queue
  also finishes, so a file the import root cannot show does not hold the
  acquisition until it times out. The reported files are the ones found, each
  with its wanted track's index.
- Progress mirrors the queue: `running` is answered only while a queue item is
  downloading and its byte count has advanced since the previous poll. The
  album search and grab, queue items that are queued, paused, delayed or
  unavailable, and Lidarr's import phase answer `queued`, so the server's stall
  window never counts a phase in which Lidarr moves no bytes.
- A `downloadFailed` history event for the album, at or after the search, fails
  the acquisition. Older failures are ignored.
- The `searchWaitMinutes` deadline uses the plugin's system clock, which the
  WASI runtime provides, so it always applies.
- `cancel-download` forgets the plugin's state and changes nothing in Lidarr:
  the queue item, the download client's transfer and Lidarr's files stay as
  they are.

## Layout

| Path | Contents |
|---|---|
| `Cargo.toml` | Crate metadata and the SDK dependency. |
| `src/lib.rs` | The plugin: configuration, lifecycle and the acquisition interface. |
| `src/api.rs` | The Lidarr HTTP transport and the trait the download logic uses. |
| `src/clock.rs` | The system clock, ISO timestamp parsing, and the wait arithmetic. |
| `src/config.rs` | The instance configuration and its schema-level checks. |
| `src/download.rs` | Ensuring the artist, monitoring and searching, polling, and completion. |
| `src/lidarr.rs` | Serde types for the Lidarr API, and the history failure rule. |
| `src/quality.rs` | The quality cutoff to candidate format mapping. |
| `src/random.rs` | Keys for the plugin's store entries. |
| `src/search.rs` | Lookup terms, candidates, and their file names. |
| `src/tokens.rs` | The opaque token codec. |
| `src/url.rs` | URL validation, joining and encoding. |
| `manifest.json` | The plugin manifest the server reads. |
| `src/fixtures/` | Recorded Lidarr API responses used by the unit tests. |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
