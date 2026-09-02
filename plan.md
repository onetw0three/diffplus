# Diffplus --- Implementation Plan

## 1. Goal

Build a Rust-first CLI that compares two software artifacts
(directories, tarballs, ZIPs, JARs, release bundles, etc.) and produces
useful, Git-style diffs based on the *semantic contents* of each file
rather than merely comparing container bytes.

The tool should recursively inspect artifacts, choose an appropriate
analyzer for each file, and write separate diff files while preserving
the original path hierarchy. The output format should also be
intentionally easy for a future Web UI to index and navigate.

Working name in this plan: `diffplus`.

## 2. Core Principles

-   Rust is the main orchestrator and CLI implementation.
-   Do not extract ordinary archives to persistent disk. Stream/read
    archive members and represent them through a virtual filesystem.
-   External analyzers may use ephemeral workspaces when their tooling
    requires filesystem paths.
-   IDA Pro is supplied from a host directory and mounted read-only into
    the Docker container. Never bundle IDA into the distributable image.
-   Treat JADX and IDA/Diaphora as analyzer backends, not as part of the
    core diff implementation.
-   Hash before expensive analysis and cache expensive analyzer results.
-   Normalize analyzer output before diffing to reduce noise.
-   Preserve provenance: every generated diff must map clearly back to
    its artifact and original path.
-   Prefer deterministic output so two identical runs produce the same
    output tree.

## 3. Initial Scope

Inputs:

-   Directories
-   `.tar`
-   `.tar.gz` / `.tgz`
-   `.tar.bz2`
-   `.tar.xz`
-   `.zip`
-   `.jar`
-   Nested supported archives

File handling:

-   Text files: normal unified/Git-style diff.
-   Identical binary files: no diff.
-   JAR/JVM artifacts: recursively inspect and/or decompile using JADX,
    then diff normalized source.
-   Native binaries such as ELF `.so`, executables, PE `.exe`/`.dll`,
    etc.: analyze using IDA + Hex-Rays + Diaphora and diff matched
    decompilation.
-   Unsupported binary formats: report as binary changed, with hashes
    and metadata.

The architecture should make `.war`, `.aar`, `.apk`, `.deb`, `.rpm`,
WASM, Ghidra, Binary Ninja, etc. possible later without redesigning the
core.

## 4. High-Level Architecture

``` text
old artifact                 new artifact
     |                            |
     +---------- scanner ---------+
                    |
             Virtual Artifact Tree
                    |
             path correspondence
                    |
              cheap hash check
                    |
          +---------+---------+
          |                   |
       identical            changed
          |                   |
        skip            classify/analyze
                              |
             +----------------+----------------+
             |                |                |
           text              JVM             native
             |                |                |
        text diff            JADX       IDA + Diaphora
             |                |                |
             +----------------+----------------+
                              |
                       normalized text/tree
                              |
                         diff renderer
                              |
                         output tree
```

The core should not know how JADX, IDA, or Diaphora work internally. An
analyzer receives an artifact and returns a normalized representation
that the diff layer understands.

## 5. Rust Project Layout

Suggested workspace:

``` text
diffplus/
├── Cargo.toml
├── crates/
│   ├── diffplus-core/
│   ├── diffplus-vfs/
│   ├── diffplus-archives/
│   ├── diffplus-classify/
│   ├── diffplus-diff/
│   ├── diffplus-cache/
│   ├── analyzer-jadx/
│   ├── analyzer-native/
│   └── diffplus-cli/
├── ida/
│   ├── scripts/
│   └── README.md
├── docker/
│   ├── Dockerfile
│   └── entrypoint.sh
└── tests/
    └── fixtures/
```

Do not over-split crates during the first implementation if that slows
development. The important separation is conceptual: VFS/scanning,
analyzers, comparison, rendering, caching, and CLI.

## 6. Virtual Artifact Tree

The central abstraction should represent files without requiring them to
exist as extracted host files.

Conceptually:

``` rust
struct ArtifactEntry {
    logical_path: ArtifactPath,
    kind: ArtifactKind,
    size: u64,
    digest: Digest,
    metadata: Metadata,
    content: ContentSource,
}
```

`ContentSource` should permit streaming/read-on-demand instead of
requiring `Vec<u8>` for every file.

Possible artifact kinds:

``` rust
enum ArtifactKind {
    Text,
    Archive,
    Jar,
    JavaClass,
    NativeBinary,
    Binary,
    Symlink,
    Directory,
}
```

Archive paths should be represented independently of host paths. For
nested containers, retain provenance such as:

``` text
release.tar.gz!/lib/app.jar!/com/example/Main.class
```

while also supporting a clean logical tree for output.

Reject unsafe archive paths such as `../...`.

## 7. Archive Processing

Implement recursive archive readers.

Requirements:

-   No ordinary archive extraction to persistent disk.
-   Stream entries where practical.
-   Normalize separators to `/`.
-   Detect duplicate paths.
-   Defend against path traversal.
-   Apply configurable recursion limits.
-   Apply configurable expanded-size limits to defend against archive
    bombs.
-   Apply configurable per-file size limits.
-   Hash members while reading.
-   Avoid reading both entire releases into RAM.

For archives that contain one versioned top-level directory, consider an
optional top-level stripping mode:

``` text
package-1.2/src/foo.c
package-1.3/src/foo.c
```

can compare as:

``` text
src/foo.c
```

This should be explicit/configurable rather than silently guessed in
ambiguous cases.

## 8. Analyzer Interface

Use a generic analyzer contract.

Conceptually:

``` rust
trait Analyzer {
    fn supports(&self, entry: &ArtifactEntry) -> bool;

    fn analyze(
        &self,
        ctx: &AnalysisContext,
        entry: &ArtifactEntry,
    ) -> Result<AnalysisOutput>;
}
```

`AnalysisOutput` should support both a single normalized text
representation and a virtual subtree:

``` rust
enum AnalysisOutput {
    Text(NormalizedText),
    Tree(VirtualTree),
    Binary(BinaryMetadata),
}
```

This allows:

-   text -\> text
-   JAR -\> Java source tree
-   native binary -\> function pseudocode tree
-   unknown binary -\> metadata

## 9. Text Diff Backend

Produce unified/Git-style diffs with ANSI color when printing to a
terminal.

CLI color behavior:

``` text
--color auto
--color always
--color never
```

When writing `.diff` files, default to no ANSI escapes.

Diff semantics should approximately follow Git:

-   exit `0`: no differences
-   exit `1`: differences found
-   exit `2+`: execution/error
-   additions/deletions/modifications
-   `/dev/null` for added/deleted files
-   configurable context lines

If practical, use a mature Rust diff library rather than implementing
the diff algorithm manually.

## 10. JADX Backend

JAR/JVM handling should produce source-oriented diffs.

Example:

``` text
release.tar.gz
└── app.jar
    └── com/example/Main.class
```

becomes conceptually:

``` text
app.jar/
└── com/example/Main.java
```

and changes produce Java source diffs instead of:

``` text
Binary files app.jar differ
```

JADX should remain an external process/backend.

The analyzer should:

1.  Detect JAR/JVM artifacts.
2.  Skip analysis if the artifact hash is identical.
3.  Materialize input only if JADX requires a path.
4.  Invoke JADX in an isolated ephemeral workspace.
5.  Capture generated source.
6.  Normalize deterministic noise where needed.
7.  Return a virtual source tree.
8.  Destroy the workspace after results are captured.
9.  Cache analysis by content hash + analyzer version/options.

Consider recursively treating JAR as ZIP first so resources, manifests,
and non-class contents can also be compared.

## 11. Native Binary Backend

Native binaries should use IDA Pro + Hex-Rays + Diaphora.

The objective is not to diff raw addresses or binary bytes. It is to
identify corresponding functions and diff their pseudocode.

Example:

``` text
old: sub_140001230 @ 0x140001230
new: sub_18000A940 @ 0x18000A940
```

should become a matched function and produce something like:

``` diff
diff --git a/libfoo.so/functions/foo_init.c b/libfoo.so/functions/foo_init.c
@@ ...
- return old_value;
+ return new_value;
```

Addresses are metadata, not identity.

### Diaphora Integration

Use the existing `diaphora-mcp` project as a reference/fork candidate,
but extract its useful IDA/Diaphora orchestration into a library/backend
rather than requiring the main program to communicate over MCP.

Target separation:

``` text
Diaphora integration
├── core/
│   ├── export
│   ├── match
│   └── results
├── ida/
│   └── headless invocation
├── API/library adapter
└── optional MCP wrapper
```

The Rust application can invoke a small Python/IDA-side adapter if that
is the cleanest integration. Do not force a Rust rewrite of
IDAPython/Diaphora internals.

Target result model:

``` rust
struct FunctionDiff {
    stable_id: String,
    old_address: Option<u64>,
    new_address: Option<u64>,
    old_name: Option<String>,
    new_name: Option<String>,
    status: FunctionStatus,
    similarity: Option<f64>,
    old_pseudocode: Option<String>,
    new_pseudocode: Option<String>,
    metadata: FunctionMetadata,
}
```

Statuses:

``` text
added
deleted
modified
unchanged
unresolved
```

Diaphora should perform the difficult cross-version function matching
rather than reimplementing that initially.

## 12. Pseudocode Normalization

Before diffing Hex-Rays output, normalize noise where safe.

Candidates:

-   raw function addresses
-   generated labels tied only to addresses
-   unstable compiler/decompiler temporary names
-   generated global names tied only to addresses
-   irrelevant analysis metadata

Do **not** normalize semantically meaningful constants or logic.

For example:

``` c
if (x == 42)
```

versus:

``` c
if (x == 43)
```

must remain a visible change.

Keep the original pseudocode available in metadata/cache if
normalization is applied so results can be audited.

## 13. Uncertain Function Matches

Do not silently force low-confidence native function matches.

Represent ambiguity:

``` text
old sub_401230
possible:
  new sub_701A10  similarity 0.81
  new sub_701D20  similarity 0.79
```

The output manifest should retain matching confidence and
reasons/metadata when Diaphora exposes them.

A future Web UI should be able to display these candidates.

## 14. Docker Model

IDA will always exist in a local host directory.

Do not copy IDA into the Docker image.

Example deployment:

``` bash
docker run --rm \
  -v /opt/ida:/opt/ida:ro \
  -v "$PWD":/work:ro \
  -v diffplus-cache:/cache \
  diffplus \
  /work/old.tar.gz \
  /work/new.tar.gz \
  --output /output
```

Configuration:

``` text
IDA_PATH=/opt/ida
```

or:

``` text
--ida-path /opt/ida
```

The container should contain:

``` text
Rust diffplus executable
Python runtime if Diaphora requires it
Diaphora integration
JADX
supporting libraries
```

The IDA directory is a read-only host mount.

## 15. Ephemeral Workspaces

The project should redefine "diskless" as:

> No persistent extraction or analysis artifacts unless explicitly
> requested.

Ordinary archive traversal stays in memory/streaming.

Tools that require files get a private ephemeral workspace:

``` text
/tmp/diffplus/<job-id>/
├── input.bin
├── ida/
├── old.sqlite
├── new.sqlite
└── diff.sqlite
```

After the normalized result is captured:

``` text
delete workspace
```

When Docker itself is disposable, container destruction provides another
cleanup boundary.

Never write generated IDA databases into the mounted host IDA directory.

## 16. Output Layout

Rather than one enormous diff, write separate diff files while
preserving source paths.

Example input:

``` text
release/
├── etc/app.yaml
├── app/backend.jar
└── lib/libfoo.so
```

Suggested output:

``` text
output/
├── manifest.json
├── summary.json
├── diffs/
│   ├── etc/
│   │   └── app.yaml.diff
│   ├── app/
│   │   └── backend.jar/
│   │       └── com/
│   │           └── example/
│   │               └── Main.java.diff
│   └── lib/
│       └── libfoo.so/
│           └── functions/
│               ├── foo_init.c.diff
│               ├── foo_open.c.diff
│               └── fn_8f3c91a2.c.diff
└── metadata/
    ├── app/
    │   └── backend.jar.json
    └── lib/
        └── libfoo.so.json
```

This layout is intentionally Web-UI-friendly.

Avoid unsafe filesystem characters and collisions when mapping logical
paths to output paths.

## 17. Manifest

A machine-readable manifest is essential even for the first CLI version.

`manifest.json` should contain enough information for a future UI
without rescanning diff files.

Example shape:

``` json
{
  "schema_version": 1,
  "old": {
    "name": "release-1.tar.gz",
    "sha256": "..."
  },
  "new": {
    "name": "release-2.tar.gz",
    "sha256": "..."
  },
  "stats": {
    "added": 3,
    "deleted": 1,
    "modified": 12,
    "unchanged": 941
  },
  "entries": [
    {
      "path": "lib/libfoo.so/functions/foo_init.c",
      "container_path": "lib/libfoo.so",
      "kind": "native_function",
      "status": "modified",
      "diff": "diffs/lib/libfoo.so/functions/foo_init.c.diff",
      "similarity": 0.97,
      "old_address": "0x401230",
      "new_address": "0x7a9230"
    }
  ]
}
```

Version the schema from day one.

## 18. Summary Output

Generate a compact summary suitable for terminal use:

``` text
release-1.tar.gz -> release-2.tar.gz

941 unchanged
 12 modified
  3 added
  1 deleted
  2 binary/unresolved

Text                 5 changed
JADX                 4 changed
IDA/Diaphora         7 changed

Output: ./diffplus-output/
```

Optionally support:

``` text
--format text
--format json
```

for stdout.

## 19. Future Web UI

Do not implement the Web UI in phase one, but design outputs for it now.

The Web UI should eventually be able to:

-   Navigate the artifact hierarchy.
-   Filter added/deleted/modified files.
-   Search paths/functions.
-   Display side-by-side or unified diffs.
-   Collapse unchanged sections.
-   Navigate JAR packages/classes.
-   Navigate native binaries/functions.
-   Display Diaphora similarity/confidence.
-   Show old/new addresses as metadata.
-   Show unresolved function matches.
-   Show analyzer used and analyzer version.
-   Link a diff back to its containing artifact.
-   Display summary/statistics.
-   Potentially compare original versus normalized pseudocode.

The Web UI should consume `manifest.json` and diff files rather than
rerunning analysis.

This makes the analysis engine usable independently in CI.

## 20. Caching

Caching is likely a bigger performance win than choosing Rust over
Python.

Compute a cryptographic digest for every artifact/member.

Basic rule:

``` text
old hash == new hash
    -> unchanged
    -> do not analyze
```

Cache expensive analyzer results by:

``` text
content hash
+ analyzer name
+ analyzer version
+ relevant analyzer options
+ normalization version
```

For example:

``` text
/cache/
└── ida/
    └── <cache-key>/
        ├── metadata.json
        └── analysis...
```

and:

``` text
/cache/
└── jadx/
    └── <cache-key>/
        └── ...
```

Do not cache sensitive analysis by default unless the cache
behavior/location is explicit and documented.

Support:

``` text
--cache-dir
--no-cache
```

## 21. Concurrency

Rust should orchestrate expensive jobs concurrently, with limits.

Example:

``` text
--jobs 8
--ida-jobs 2
--jadx-jobs 4
```

IDA concurrency should have its own limit because of CPU/RAM/license
constraints.

Do not spawn one analyzer process per file without bounds.

Pipeline work where possible:

``` text
scan -> hash -> classify -> enqueue analysis -> render
```

## 22. Security

Assume all compared artifacts may be hostile.

Requirements:

-   Path traversal protection.
-   Archive recursion limits.
-   Expanded-size limits.
-   Per-entry size limits.
-   Timeouts for external analyzers.
-   Memory/CPU limits where practical.
-   No shell interpolation for subprocess invocation.
-   Random/private temporary directories.
-   Read-only IDA mount.
-   Cleanup on success, failure, timeout, and signals where possible.
-   Avoid following symlinks out of the artifact root.
-   Never execute binaries being analyzed.
-   Consider a separate restricted worker/container for native analysis
    later.

## 23. CLI Sketch

Initial CLI:

``` bash
diffplus OLD NEW
```

Useful options:

``` text
-o, --output <dir>
--color auto|always|never
--context <lines>
--jobs <n>
--ida-jobs <n>
--jadx-jobs <n>
--ida-path <path>
--jadx-path <path>
--cache-dir <path>
--no-cache
--max-depth <n>
--max-file-size <bytes>
--strip-top-level
--keep-workdir
--verbose
```

Analyzer controls:

``` text
--native auto|ida|raw|off
--jvm auto|jadx|raw|off
```

`--keep-workdir` should exist primarily for debugging and be clearly
documented as persisting intermediate files.

## 24. Analyzer Selection

Classification should use both extension and content/magic bytes.

Do not trust extension alone.

Example selection:

``` text
UTF/text                       -> TextAnalyzer
ZIP/JAR                        -> Archive/JarAnalyzer
.class / DEX / JVM artifact    -> JadxAnalyzer where supported
ELF                            -> NativeAnalyzer
PE                             -> NativeAnalyzer
Mach-O                         -> NativeAnalyzer
unknown binary                 -> RawBinaryAnalyzer
```

Analyzers should be configurable and replaceable.

## 25. Native Result Representation

For each binary, consider emitting a virtual tree:

``` text
libfoo.so/
├── functions/
│   ├── foo_init.c
│   ├── foo_open.c
│   └── fn_8f3c91a2.c
└── metadata.json
```

Named symbols should be used where stable.

For unnamed functions, do not derive the final identity solely from the
complete function body hash because a one-line modification would make
the function appear deleted/added.

Let Diaphora matching establish correspondence and assign a
deterministic output identifier for that comparison.

## 26. Performance Strategy

Rust is chosen primarily for:

-   streaming I/O
-   bounded memory use
-   concurrency
-   efficient hashing
-   robust CLI packaging
-   type-safe architecture

Do not expect Rust itself to make IDA or JADX substantially faster.

Performance priorities should be:

1.  Skip identical files by hash.
2.  Cache expensive analyses.
3.  Avoid unnecessary decompilation.
4.  Stream archives.
5.  Bound concurrency intelligently.
6.  Only then optimize CPU-heavy Rust code.

## 27. Error Handling

A failure to analyze one file should not necessarily destroy the entire
release comparison.

Represent per-entry failures:

``` json
{
  "path": "lib/broken.so",
  "status": "analysis_error",
  "analyzer": "ida-diaphora",
  "error": "IDA analysis timed out"
}
```

CLI should summarize partial failures and return a distinct exit code
when analysis was incomplete.

Suggested exit semantics:

``` text
0 = no differences
1 = differences found
2 = fatal tool/input error
3 = comparison completed with analysis errors
```

Exact semantics can be refined, but document them and test them.

## 28. Logging

Logs go to stderr.

Diff/structured output goes to stdout or the output directory.

Support levels such as:

``` text
error
warn
info
debug
trace
```

Never mix diagnostic logs into `.diff` contents.

## 29. Testing Strategy

Create small deterministic fixtures for:

-   identical text archives
-   modified text file
-   added/deleted files
-   nested ZIP/TAR
-   path traversal archive
-   duplicate archive members
-   archive recursion limits
-   binary unchanged
-   binary changed
-   JAR with one Java method changed
-   native binary with one function changed
-   native function address movement without semantic change
-   native function semantic modification
-   native added/deleted functions
-   low-confidence/unresolved native match
-   analyzer timeout
-   cache hit/miss
-   deterministic output paths
-   Docker-mounted IDA integration

Keep IDA-dependent integration tests separate from normal unit tests so
contributors without IDA can run the core test suite.

## 30. Implementation Phases

### Phase 1 --- Core CLI

Implement:

-   Rust CLI
-   directory comparison
-   TAR/ZIP support
-   virtual paths
-   hashing
-   text classification
-   unified diff
-   output tree
-   `manifest.json`
-   summary
-   basic safety limits

Deliverable:

``` bash
diffplus old.tar.gz new.tar.gz -o result/
```

produces useful per-file diffs.

### Phase 2 --- Recursive Containers

Implement:

-   nested archives
-   JAR-as-archive handling
-   recursion controls
-   provenance paths
-   better binary classification

### Phase 3 --- JADX

Implement:

-   external JADX adapter
-   ephemeral workspace
-   source virtual tree
-   Java diff output
-   analyzer cache

### Phase 4 --- IDA/Diaphora

Investigate/fork `diaphora-mcp`.

Extract:

-   headless IDA invocation
-   Diaphora export
-   database comparison
-   matched-function queries
-   pseudocode retrieval

Create a narrow adapter returning structured function results.

Then integrate it into the Rust native analyzer.

### Phase 5 --- Docker

Implement container packaging with:

``` text
/opt/ida -> host-mounted read-only IDA
/cache   -> optional persistent cache
/work    -> read-only input
/output  -> writable result
```

Document host requirements.

### Phase 6 --- Hardening

Add:

-   resource limits
-   timeouts
-   cancellation
-   improved normalization
-   bounded worker pools
-   robust cleanup
-   large archive testing
-   malformed input fuzzing where practical

### Phase 7 --- Agent Integration

Expose a bounded stdio MCP server that reuses the CLI configuration and result
contract. Agents should be able to:

-   compare two local artifacts into an explicit output directory
-   page and filter changed manifest entries
-   retrieve individual unified diffs without loading an entire result

Keep protocol output isolated on stdout, send progress to stderr, advertise
destructive operations accurately, and validate persisted result paths.

### Phase 8 --- Web UI

Build the Web UI against the versioned manifest/output contract.

The analysis engine should not need architectural changes for the UI.

## 31. First Agent Tasks

An implementation agent should start with these tasks in order:

1.  Create the Rust workspace and CLI.
2.  Define `ArtifactPath`, `ArtifactEntry`, `ContentSource`, `Analyzer`,
    and `AnalysisOutput`.
3.  Implement directory, TAR, and ZIP readers.
4.  Implement hashing and cheap unchanged detection.
5.  Implement text detection and unified diff generation.
6.  Implement the path-preserving output writer.
7.  Define and write `manifest.json` schema version 1.
8.  Add fixtures and core tests.
9.  Add recursive archive handling with safety limits.
10. Implement analyzer process abstraction for external tools.
11. Add JADX.
12. Independently prototype the Diaphora/IDA adapter before coupling it
    tightly to the Rust core.
13. Integrate native analysis only after the adapter can return stable
    structured JSON/function records.
14. Add Docker packaging.
15. Benchmark and add caching/concurrency based on measured bottlenecks.

## 32. Key Design Decision

The central invariant should be:

> Every artifact is transformed into either a comparable text
> representation, a comparable virtual subtree, or explicit binary
> metadata.

The diff engine should not care whether that representation came from:

``` text
a source file
a TAR member
a JAR decompiled by JADX
a native function decompiled by Hex-Rays
or a future analyzer
```

That separation is what will keep the project extensible and make the
future Web UI straightforward.
