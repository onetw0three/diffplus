# Manifest schema versions

`manifest.json` is the stable index for one artifact comparison. The current
schema version is `3`.

- `schema_version`: currently `3`.
- `old` and `new`: input display name and SHA-256 digest.
- `stats`: counts of added, deleted, modified, and unchanged entries.
- `entries`: deterministic path-ordered comparison records.

Each entry contains its logical `path`, semantic `kind`, `status`, optional
relative `diff` path, and old/new SHA-256 digests. Version 2 added optional
`old_content` and `new_content` paths for changed text files. These point to
deduplicated `blobs/<sha256>` snapshots and support durable side-by-side views.
Version 3 adds `old_path`, `new_path`, `renamed`, `old_size`, and `new_size`.
The `renamed` statistic is orthogonal to `modified`: a path can be both renamed
and content-modified. Consumers must ignore unknown fields.

The built-in terminal viewer consumes only this manifest and its referenced
relative paths. It validates those paths, loads selected content lazily, and
can therefore reopen a result independently of the original artifacts.

Statuses are `added`, `deleted`, `modified`, `renamed`, and `unchanged`.
Native comparisons also emit `native-functions.json`, a protocol-versioned
sidecar containing function addresses, names, confidence, match category,
reason, status, and pseudocode. Keeping this analyzer-specific data outside
the core manifest preserves compatibility. Per-entry analyzer failures will require
a future manifest schema extension.
