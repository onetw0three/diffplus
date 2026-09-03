# diffplus

Diffplus compares software releases by semantic content instead of only
comparing container bytes. It is a Rust CLI with a VS Code-style terminal
viewer, optional JADX and IDA/Diaphora analyzers, and a stdio MCP server for
agent-driven patch-diff workflows.

## Features

- Compare files, directories, ZIPs, JARs, TARs, gzip, bzip2, and xz streams.
- Stream and index large inputs on disk instead of retaining every file in RAM.
- Produce deterministic unified diffs and a versioned JSON manifest.
- Detect unique moves and versioned filename changes conservatively.
- Decompile changed JARs with JADX, eagerly or on demand.
- Match and compare native functions with IDA Pro, Hex-Rays, and Diaphora.
- Browse results in a mouse-aware, resizable terminal interface.
- Expose bounded comparison and result-reading tools through MCP.
- Bound archive depth, member size, total expansion, and viewer/MCP responses.

The native pipeline has been exercised successfully with IDA Pro 9.4,
Diaphora 3.4.2, and the x64 Hex-Rays decompiler. A small `/usr/bin/true` versus
`/usr/bin/false` run produced 91 function records and 2 modified functions.

## Requirements

The base application needs:

- A current stable Rust toolchain with Cargo.
- A platform supported by the Rust dependencies. Development and analyzer
  integration are currently tested on 64-bit Linux.
- Temporary disk space for changed archive members and analyzer workspaces.

Optional analyzers need:

- JADX plus a 64-bit Java runtime for JVM source-level diffs. Java 17 or newer
  is recommended; upstream JADX documents Java 11 as its minimum.
- IDA Pro, an appropriate Hex-Rays decompiler, Diaphora, and Python 3 for
  native function-level diffs.

IDA Pro and Diaphora are not bundled with Diffplus. Ensure your use of those
components complies with their respective licences.

## Build and install

Clone over SSH and build the optimized executable:

```bash
git clone git@github.com:onetw0three/diffplus.git
cd diffplus
cargo build --release
```

The executable is now `target/release/diffplus`. Debug builds work but are
substantially slower for hashing, decompression, and large comparisons.

Optionally install the executable into Cargo's binary directory:

```bash
cargo install --path .
diffplus --version
diffplus --help
```

If `~/.cargo/bin` is not already on `PATH`, either add it or invoke the release
binary by its absolute path.

## Quick start

`OLD` and `NEW` can independently be regular files, directories, or supported
archives:

```bash
diffplus OLD NEW --output result
```

Examples:

```bash
diffplus old-directory new-directory --output result
diffplus old.tar.gz new.tar.gz --output result
diffplus old.zip new.zip --output result --tui
```

The output directory is replaced transactionally only after a successful run.
A previous result remains in place if generation fails.

Exit codes are:

- `0`: no differences.
- `1`: differences found. This is a normal comparison result, not a failure.
- `2`: invalid input, configuration, or analyzer failure.

Progress, analyzer output, and 30-second subprocess heartbeats are written to
stderr. `--quiet` suppresses progress without hiding errors or the final
summary.

## Configuration

Diffplus automatically reads:

```text
$XDG_CONFIG_HOME/diffplus/config.toml
```

When `XDG_CONFIG_HOME` is unset, it reads:

```text
~/.config/diffplus/config.toml
```

Create the directory with:

```bash
mkdir -p ~/.config/diffplus
```

Every field is optional. This example shows all persistent settings supported
by the current configuration loader:

```toml
output = "result"
tui = false
color = "auto"
context = 3

max_file_size = 104857600
max_expanded_size = 2147483648
max_depth = 1
strip_top_level = false

jvm = "raw"
jadx_path = "/opt/jadx/bin/jadx"

native = "auto"
ida_path = "~/ida-pro-9.4/idat"
diaphora_path = "~/diaphora"
diaphora_script = "~/diffplus/ida/scripts/diaphora_adapter.py"
python_path = "python3"

cache_dir = "~/.cache/diffplus"
workspace_dir = "/tmp/diffplus"
no_cache = false
quiet = false
```

Command-line arguments override configured values. Leading `~/` path
components are expanded. Relative paths retain the same current-directory
semantics as CLI paths. Unknown or misspelled keys are rejected.

To disable either byte limit in the configuration file, set it to `false` or
the string `"none"`:

```toml
max_file_size = false
max_expanded_size = "none"
```

TOML does not define a bare `none` value, so the quotes are required for that
form. Disabling these limits removes an important archive-bomb safeguard; use
it only with trusted inputs and sufficient disk space.

Use a different file or disable configuration entirely with:

```bash
diffplus --config /path/to/config.toml OLD NEW
diffplus --no-config OLD NEW
```

The MCP server loads the same configuration, so analyzer paths and safety
limits do not need to be repeated in the MCP host configuration.

## JADX setup

JADX is optional because the default `--jvm raw` mode keeps the initial archive
scan fast. A changed JAR remains available in the result and can be decompiled
by pressing `Enter` in the TUI.

Install a current JADX release from the
[JADX releases page](https://github.com/skylot/jadx/releases). A typical Linux
layout is:

```text
/opt/jadx/bin/jadx
/opt/jadx/lib/...
```

Verify Java and JADX:

```bash
java -version
/opt/jadx/bin/jadx --version
```

Diffplus discovers JADX in this order:

1. The configured or explicitly supplied `--jadx-path`.
2. `PATH`.
3. `$JADX_HOME/bin/jadx`.
4. `/opt/jadx/bin/jadx`.
5. `/usr/local/bin/jadx`.
6. `~/tools/bin/jadx`.

JVM modes are:

- `--jvm raw` (default): retain changed JARs for on-demand analysis.
- `--jvm auto`: use JADX when it can be discovered.
- `--jvm jadx`: require immediate source-level decompilation.
- `--jvm off`: disable JVM-specific analysis.

Run an eager JAR comparison with:

```bash
diffplus old.jar new.jar \
  --jvm jadx \
  --jadx-path /opt/jadx/bin/jadx \
  --cache-dir ~/.cache/diffplus \
  --output result-jadx
```

Generated source formatting and JADX metadata comments are normalized before
diffing to reduce decompiler noise.

## IDA Pro and Diaphora setup

Native function matching is optional. Diffplus currently uses headless IDA
processes rather than linking IDAlib into the main executable. This keeps the
base binary portable and preserves process, memory, and failure isolation.

### 1. Install IDA and Hex-Rays

Install IDA Pro and the decompiler modules needed for the target architectures.
For IDA Pro 9.4 on Linux, use the lower-resource terminal executable for batch
analysis:

```text
~/ida-pro-9.4/idat
```

Older IDA layouts may use `ida64` or `idat64`; configure the executable that is
present in your installation. Verify IDA and the desired decompiler modules:

```bash
~/ida-pro-9.4/idat -h
ls ~/ida-pro-9.4/plugins/hexx64.so
ls ~/ida-pro-9.4/plugins/hexarm.so
```

The current host has `hexx64.so` and `hexarm.so`. Other architectures require
the corresponding licensed Hex-Rays module.

### 2. Install Diaphora

Diffplus uses the Diaphora source directory directly; it does not need to copy
Diaphora into this repository. The tested setup uses release 3.4.2:

```bash
git clone --depth 1 --branch 3.4.2 \
  https://github.com/joxeankoret/diaphora.git \
  ~/diaphora
```

The directory must contain at least `diaphora.py` and `diaphora_ida.py`.
Diaphora's `cdifflib` and machine-learning packages are optional for the
baseline Diffplus workflow. Missing packages can produce warnings but do not
prevent export and matching. Avoid bypassing an operating system's
externally-managed Python protections; use a virtual environment if you later
choose to install optional packages, then set `python_path` to that environment.

### 3. Configure Diffplus

Add the following paths to `~/.config/diffplus/config.toml`:

```toml
native = "auto"
ida_path = "~/ida-pro-9.4/idat"
diaphora_path = "~/diaphora"
diaphora_script = "~/diffplus/ida/scripts/diaphora_adapter.py"
python_path = "python3"
cache_dir = "~/.cache/diffplus"
```

`native = "auto"` keeps ordinary comparisons usable if the complete native
analyzer configuration is unavailable. Use `--native ida` when native analysis
must run and an incomplete or invalid setup should be fatal.

### 4. Run a small smoke test

On Linux, this gives a quick end-to-end check of IDA export, Hex-Rays
pseudocode, Diaphora matching, and Diffplus output:

```bash
diffplus /usr/bin/true /usr/bin/false \
  --native ida \
  --output result-ida-smoke
```

Expect exit code `1` because the files differ. Inspect the result with:

```bash
diffplus --view result-ida-smoke
```

A native result includes `native-functions.json` and function diffs under
`diffs/functions/`. The sidecar retains old/new addresses and names, status,
similarity, Diaphora match category and reason, and available pseudocode.
Unreliable and multi-match records are marked unresolved instead of being
silently accepted.

Native modes are:

- `--native auto` (default): use IDA/Diaphora when all paths are configured;
  otherwise retain a raw binary comparison.
- `--native ida`: require IDA/Diaphora.
- `--native raw` or `--native off`: skip native function analysis.

Direct native file pairs can be analyzed eagerly. Native binaries inside large
containers are retained for on-demand TUI analysis so an archive scan does not
launch IDA for every binary. The TUI also permits manually pairing unmatched
ELF, PE, or Mach-O files before invoking IDA/Diaphora.

## Archive depth and resource safety

The default `--max-depth 1` expands only the top-level archive, or archives
encountered directly in an input directory. Embedded archives remain ordinary
binary entries. Opt into deeper expansion explicitly:

```bash
diffplus old.tar new.tar --max-depth 2 --output result
```

Expansion is guarded by:

- `--max-file-size`: maximum uncompressed size accepted for one member.
- `--max-expanded-size`: maximum total uncompressed bytes visited per input.
- `--max-depth`: maximum archive expansion levels; minimum 1.

The defaults are 64 MiB per file, 1 GiB total per input, and one level deep.
Each byte limit can be disabled in the configuration as described above.
These checks apply while streaming and protect against path traversal and
common archive/ZIP-bomb behavior. The larger limits in the configuration
example are suitable only when the input is trusted and the host has enough
disk capacity.

Top-level uncompressed TAR members are represented as source byte ranges,
which avoids copying them into a workspace. Directory files are referenced in
place. Archive and analyzer content is staged into temporary disk-backed,
content-addressed files as needed.

Use `--workspace-dir` to place ephemeral work on a filesystem with sufficient
space. Workspaces are removed after a run. Do not place the output or workspace
inside either input directory; Diffplus rejects those layouts.

## Caching and performance

For large comparisons:

- Use the release build.
- Keep `--max-depth 1` unless nested content is required.
- Leave JVM analysis in `raw` mode and invoke JADX from the TUI only where
  useful.
- Configure `cache_dir` to reuse JADX output and per-binary Diaphora export
  databases by content and analyzer fingerprint.
- Place `workspace_dir` on a fast disk with enough free space.
- Use `--no-cache` for a clean analyzer run.

Native export caches may contain sensitive analysis data and are therefore
opt-in. Cache and result directories are not automatically deleted; manage
their retention according to available storage. Ephemeral workspaces are
cleaned automatically.

## Result layout

A normal result resembles:

```text
result/
├── manifest.json
├── summary.txt
├── blobs/
│   └── <sha256>
└── diffs/
    └── <logical-path>.diff
```

Native results additionally contain:

```text
native-functions.json
diffs/functions/<stable-function-id>.c.diff
```

`manifest.json` schema version 3 is the stable result index. Each deterministic
entry records its logical path, old/new paths, status, kind, rename flag,
SHA-256 digests, sizes, optional diff path, and optional old/new blob paths.
Changed text sides are retained as deduplicated `blobs/<sha256>` objects so the
TUI can reopen a result without the original inputs. Unchanged file content is
not copied.

See [`docs/manifest.md`](docs/manifest.md) for the consumer contract.

## Terminal UI

Open the viewer after comparison:

```bash
diffplus OLD NEW --output result --tui
```

Or reopen a saved result:

```bash
diffplus --view result
```

The viewer loads selected content lazily and refuses individual payloads above
32 MiB. The consolidated analyzer view is limited to 64 MiB.

| Input | Action |
| --- | --- |
| `q` | Quit |
| `/` | Search paths; `Enter` or `Esc` leaves search |
| `Up`/`Down`, `j`/`k` | Move through the explorer |
| `Space`, `Right`, `Left` | Expand or collapse folders |
| `Enter` | Open a folder, invoke analysis, or enter analyzer details |
| `Backspace` | Return from an analyzer child view |
| `Tab` or `u` | Toggle side-by-side and unified views |
| `PageUp`/`PageDown`, `J`/`K` | Scroll vertically |
| `[`/`]` | Pan horizontally |
| `Home` | Reset vertical and horizontal scrolling |
| `1` | Toggle modified and renamed entries |
| `2` | Toggle added entries |
| `3` | Toggle deleted entries |
| `4` | Toggle unchanged entries |
| `m` | Mark or unmark one unmatched text, JAR, or native file for manual pairing |

Mouse clicks select files and toggle folders. The wheel scrolls the explorer or
diff under the pointer. Drag the explorer/editor divider or the before/after
divider to resize panels. Editor headers show old/new paths and sizes.

A changed JAR or native pair initially shows one consolidated diff on its parent
entry. Press `Enter` to open the source-file or function child explorer and
`Backspace` to return. To compare unmatched files, select one side, press `m`,
move to the counterpart, and press `Enter`. A `◆` marks the first selection.
Text files are compared directly; JAR and native pairs use JADX and
IDA/Diaphora respectively. Both files must use the same comparison type.

## MCP server

Start a local stdio Model Context Protocol server with:

```bash
/path/to/diffplus --mcp
```

Register it in an MCP host using a configuration shaped like:

```json
{
  "mcpServers": {
    "diffplus": {
      "command": "/absolute/path/to/diffplus",
      "args": ["--mcp"]
    }
  }
}
```

Use an absolute executable path because MCP hosts do not always inherit an
interactive shell's `PATH`. The server keeps JSON-RPC messages on stdout and
writes progress to stderr.

Available tools:

- `compare_artifacts`: compare two local inputs and transactionally write an
  explicitly selected result directory. Per-call options include context,
  archive depth, JVM/native mode, top-level stripping, and cache bypass.
- `list_changes`: page and filter manifest entries by status or path prefix.
  Unchanged entries are excluded by default. The page limit is 1,000.
- `read_diff`: retrieve one unified diff by logical path. The default response
  limit is 256 KiB and the maximum is 2 MiB.

`compare_artifacts` is correctly advertised as destructive because it replaces
its explicit output directory. Result-reading tools are advertised as
read-only. Tool inputs reject unknown fields, manifest reads are bounded, and
diff paths are checked against traversal and symlink escapes.

The server supports the MCP 2026-07-28 stateless lifecycle and the legacy
2025-11-25 initialization lifecycle.

## Architecture

- `src/main.rs`: process entry point and mode selection.
- `src/cli.rs`: CLI options and analyzer modes.
- `src/config.rs`: typed TOML configuration and CLI precedence.
- `src/core.rs`: bounded scanning and analyzer orchestration.
- `src/scan.rs`: directory, symlink, and recursive archive traversal.
- `src/classify.rs`: text and native-binary classification.
- `src/diff.rs`: matching, deterministic manifests, and unified diffs.
- `src/model.rs`: virtual entries and persisted manifest models.
- `src/native.rs`: validated Rust/IDA protocol, cache, and process boundary.
- `src/process.rs`: bounded subprocess output and heartbeat reporting.
- `src/output.rs`: transactional result replacement.
- `src/tui/`: lazy terminal viewer and analyzer interaction.
- `src/mcp.rs`: bounded stdio MCP server.
- `ida/scripts/diaphora_adapter.py`: IDAPython/Diaphora boundary.

The core works with disk-backed content references. It does not need to know
whether comparable text came from a regular file, a TAR member, JADX, Hex-Rays,
or a future analyzer.

## Development and verification

Run the Rust tests, lints, adapter tests, and optimized build:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
python3 -m unittest discover -s ida/tests -v
cargo build --release
```

The normal native tests use mock IDA processes and SQLite fixtures, so they do
not consume an IDA licence. Use the smoke test above when validating a real IDA
installation.

## Licence

Diffplus is licensed under the MIT licence. See [`LICENSE`](LICENSE).
