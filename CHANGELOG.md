# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
