# artifact-diff

Rust-first comparison of software artifacts by their semantic contents.

## Architecture

- `src/main.rs` is the process entry point.
- `src/cli.rs` owns command-line configuration and analyzer modes.
- `src/model.rs` owns artifact entries and the versioned manifest model.
- `src/core.rs` orchestrates bounded scanning and analyzer selection.
- `src/diff.rs` produces deterministic manifests and unified diffs.
- `src/native.rs` owns the validated Rust/IDA JSON protocol and process boundary.
- `src/output.rs` transactionally replaces complete result directories.
- `src/scan.rs` owns bounded directory, symlink, and recursive archive traversal.
- `src/tui/` owns the lazy, manifest-driven terminal result viewer.

Entries are disk-backed references rather than in-memory byte buffers.
Directory files are indexed in place and never copied into the workspace.
Archive, JADX, and native-generated content is streamed into deduplicated
temporary blobs and removed automatically after result generation.
Uncompressed TAR members use byte-range references into the original TAR, so
large TAR inputs are hashed without duplicating their contents in a workspace.

## Native analysis boundary

`ida/scripts/diaphora_adapter.py` is the only component that imports IDAPython
or Diaphora. The backend exports both binaries in separate headless IDA runs,
compares the resulting SQLite databases through Diaphora, and returns matched,
unmatched, and unresolved functions over a versioned JSON protocol. IDA
databases, exports, and match results stay in an ephemeral workspace.

## Usage

```bash
cargo run -- OLD NEW --output result
```

For large production comparisons, build and run the optimized binary; debug
builds are substantially slower at hashing and decompression:

```bash
cargo build --release
target/release/artifact-diff OLD NEW --output result
```

Add `--tui` to open the comparison immediately in an interactive terminal:

```bash
cargo run -- OLD NEW --output result --tui
```

An existing result can be reopened without the original inputs:

```bash
cargo run -- --view result
```

The viewer provides a collapsible path explorer, path search, status filters,
aligned side-by-side text, and the saved unified diff. It reads the selected
blob only when needed and refuses individual viewer payloads above 32 MiB.
Press `q` to quit, `/` to search, arrow keys or `j`/`k` to navigate, `Enter` to
expand or collapse directories or analyze a changed JAR with JADX, `Space` to
toggle a selected folder, and the mouse wheel, `PageUp`/`PageDown`, or `J`/`K`
to scroll diffs. Mouse clicks select files and toggle folders. Drag the
Explorer/editor or before/after vertical divider to resize those panels. Use
`[`/`]` to pan, `Tab` to switch views, and `1`–`4` to toggle status classes.
Press `Backspace` to return from a generated JADX source comparison. The editor
header shows before/after paths and sizes.

Rename detection first pairs unique identical-content moves, then unique paths
after normalizing semantic versions and long build numbers. Ambiguous groups
remain added/deleted to avoid silently pairing unrelated files.

Use `--workspace-dir /path/with/space` to place ephemeral extracted data on a
specific filesystem. Changed text sides are retained in the result as
content-addressed `blobs/<sha256>` files; manifest `old_content` and
`new_content` fields let a UI render persistent side-by-side views.

For repeated analyzer runs, set `--cache-dir`. JADX sources and per-binary
IDA/Diaphora export databases are cached by content and analyzer fingerprint;
`--no-cache` bypasses both caches. Native exports can contain sensitive
analysis data, so caching remains opt-in. Identical top-level archives are
hashed and scanned only once even when their filenames differ.

JVM decompilation is disabled during the initial comparison by default, keeping
large archive scans fast. Changed JAR payloads are retained in the result; press
`Enter` on one in the TUI to generate and open a cached source-level comparison.
To require JADX eagerly for JAR inputs, including JARs nested in supported
containers:

```bash
cargo run -- OLD.jar NEW.jar \
  --jvm jadx \
  --jadx-path /path/to/jadx \
  --cache-dir .cache/artifact-diff
```

JADX discovery searches `PATH`, `$JADX_HOME/bin/jadx`,
`/opt/jadx/bin/jadx`, `/usr/local/bin/jadx`, and `~/tools/bin/jadx`. Use
`--jadx-path` to override discovery explicitly.

Exit codes are `0` for no differences, `1` when differences are found, and `2`
for fatal input or analyzer errors.

Progress is written to stderr by default, including input phases, TAR/ZIP member
counts, analyzer output, 30-second subprocess heartbeats, cache activity, and
IDA/Diaphora stages. Pass `--quiet` to
suppress progress without hiding errors or the final summary. Large top-level
TAR files are streamed from disk; recursive expansion remains bounded by
`--max-file-size`, `--max-expanded-size`, and `--max-depth`.

The default `--max-depth 1` expands only the top-level archive (or archives
directly contained in an input directory). Embedded archives remain ordinary
binary entries. Use `--max-depth 2` or higher to opt into recursive expansion.

## Native function diffing

Install Diaphora separately, and provide an IDA executable with Hex-Rays. The
Diaphora source remains external and is not bundled with this project.

```bash
cargo run -- OLD.bin NEW.bin \
  --native ida \
  --ida-path /opt/ida/ida64 \
  --diaphora-path /opt/diaphora \
  --diaphora-script "$PWD/ida/scripts/diaphora_adapter.py" \
  --output result
```

Use `--python-path` when the comparison phase needs a non-default Python
interpreter. Function pseudocode diffs are written below `diffs/functions/`.
`native-functions.json` retains addresses, names, Diaphora category,
similarity, match reason, status, and original pseudocode. Unreliable and
multi-match results are marked `unresolved` rather than silently accepted.

The normal test suite uses mock IDA processes and SQLite fixtures, so it does
not require an IDA license. A real IDA/Hex-Rays/Diaphora compatibility run is
still required once those host tools are available.
