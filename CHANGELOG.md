# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- A download reports `running` only while a queue item is downloading and its
  byte count advanced since the previous poll. The album search and grab, queue
  items that are queued, paused, delayed or unavailable, a finished transfer
  waiting for its import, and Lidarr's import phase answer `queued`, so the
  server's stall window no longer counts a phase in which Lidarr moves no bytes.

## [0.1.1] - 2026-10-23

### Fixed

- Album lookup offers only the albums that name the requested album: Lidarr's
  text search matches on any word, so an unrelated album could be acquired and
  its artist added to Lidarr. Artist and title are compared without case,
  accents, punctuation or a trailing edition qualifier, and a candidate lists
  the wanted tracks the release tracklist holds when Lidarr sends one, so a
  partial edition is offered as the tracks it actually holds.
- A download finishes with the tracks Lidarr's album actually holds: the plugin
  reads the album's tracks and track files, waits only for listed tracks Lidarr
  holds, and reports only the files it finds. A deluxe request that Lidarr
  satisfies with a standard edition now ends with those tracks instead of
  waiting out `searchWaitMinutes` for tracks the album does not have.

## [0.1.0] - 2026-10-23

### Added

- The repository scaffolding: the toolchain pin, the dual MIT/Apache-2.0 license
  files, this changelog and a CI workflow that formats, lints, tests, builds,
  checks and packs the plugin.
- Configuration validation and the health probe: the manifest's configuration
  defaults, the schema-level checks, and a live system status request that
  reports a wrong URL or API key when the configuration is saved.
- Album search: lookup by release-group id or by artist and album text, the
  quality profile cutoff to candidate format mapping, one candidate file per
  wanted track, and the store entry and token that carry a search between
  polls.
- Album acquisition through Lidarr: adding a missing artist without monitoring
  its catalogue, monitoring the wanted album, starting Lidarr's album search,
  and reporting queue progress.
- Completion and failure: a download completes only when every wanted track's
  imported file is present inside the import root, and fails on a recent
  `downloadFailed` history event or when no download appeared within the
  configured wait. Cancelling only forgets the plugin's state and never touches
  Lidarr's queue or files.

[Unreleased]: https://github.com/hearthsound/plugin-lidarr/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/hearthsound/plugin-lidarr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hearthsound/plugin-lidarr/releases/tag/v0.1.0
