# Gitoxide Integration

This document records how `diff-pretty` could integrate with
`~/d/gitoxide`. The central difference from native Git integration is that
gitoxide is already Rust, already exposes diff callbacks, and already has
borrowed line-oriented APIs. We would not need a C/Rust FFI boundary or a Git
fork to experiment.

The local checkout is a gitoxide workspace with `gix` 0.86.0 and `gix-diff`
0.66.0. The important conclusion is:

> A true native integration with gitoxide is substantially easier than one with
> Git, but it would be an integration with a new `gix`/`ein` command path, not a
> drop-in replacement for the stock `git` executable.

## Executive Summary

There are three possible meanings of “integrate with gitoxide”:

1. **Use gitoxide as a repository/diff library from `diff-pretty`.**
   `diff-pretty` opens the repository, walks revisions, obtains diff events, and
   sends them directly to its renderer and native pager.
2. **Add a `diff-pretty` renderer to the gitoxide workspace.**
   A new crate translates `gix-diff` events into a shared rendering model and
   the `gix` CLI invokes the native pager in-process.
3. **Make gitoxide a full replacement for `git diff`/`git log -p`.**
   This requires gitoxide command-parity work that does not currently exist,
   especially for worktree diffs, commit patch history, rename behavior,
   attributes, submodules, binaries, merge diffs, and exact output semantics.

The recommended direction is **option 2**, with option 1 as the first
prototype. It gives us a real no-process/no-pipe path while keeping the
renderer independent of gitoxide-specific types.

## What Exists Today

### `gix-diff` already has the right lower-level shape

The `gix-diff` crate is a pure-Rust, `#![forbid(unsafe_code)]` diff library.
It exposes several useful layers:

- Tree traversal through `gix_diff::tree()` and
  `gix_diff::tree_with_rewrites()`.
- Blob diff algorithms through `gix_diff::blob`.
- Unified hunk production through `gix_diff::blob::UnifiedDiff`.
- Borrowed hunk callbacks through the `ConsumeHunk` trait.
- A `DiffLineKind` enum for context, additions, and removals.
- `HunkHeader` with old/new line starts and lengths.
- `ControlFlow::Break` support in tree traversal delegates.

The local API is visible in:

- `/Users/josh/d/gitoxide/gix-diff/src/blob/unified_diff/mod.rs:29-89`.
- `/Users/josh/d/gitoxide/gix-diff/src/blob/unified_diff/impls.rs:11-100`.
- `/Users/josh/d/gitoxide/gix-diff/src/tree/function.rs:10-42`.
- `/Users/josh/d/gitoxide/gix-diff/src/tree/visit.rs:24-95`.
- `/Users/josh/d/gitoxide/gix-diff/src/tree_with_rewrites/function.rs:24-185`.

This is already close to the event model proposed in
`NATIVE-INTEGRATION.md`, except that the events are split between tree changes
and blob hunks rather than being one complete presentation stream.

### Unified hunk callbacks are especially promising

`gix_diff::blob::UnifiedDiff` does not require the caller to accept a fully
formatted string. Its `ConsumeHunk` delegate receives:

```text
HunkHeader
[(DiffLineKind, &[u8])]
```

The line bytes are borrowed from the diff input and do not include the unified
diff prefix. That means a renderer can receive additions, removals, context,
and line-number ranges directly without reparsing `@@`, `+`, and `-` prefixes.

This maps naturally to the current renderer's word-diff path:

```text
gix hunk header + typed lines
        │
        ├── pair remove/add runs
        ├── infer word edits
        ├── apply styles and line numbers
        └── append RenderedDocument lines
```

The renderer would still need to own any bytes it retains beyond the callback.
The callback's borrowed lifetime is a useful way to avoid a serialized patch,
not a way to retain Gitoxide's buffers indefinitely.

### Tree traversal can be streaming

The lower-level tree diff function receives a `Visit` delegate and is documented
as trying to reduce allocations. It can stop traversal through
`ControlFlow::Break`. This is preferable to the convenience API when building a
large interactive document.

The higher-level `gix::Repository::diff_tree_to_tree()` method currently returns
a `Vec<ChangeDetached>`. It is convenient and useful for a first prototype, but
the native path should prefer the callback-oriented traversal to avoid buffering
all file changes before rendering.

The relevant high-level facade is in:

- `/Users/josh/d/gitoxide/gix/src/object/tree/diff/mod.rs:5-15` for traversal
  control flow and borrowed `Change` values.
- `/Users/josh/d/gitoxide/gix/src/object/tree/diff/mod.rs:13-100` for additions,
  deletions, modifications, and rewrites.
- `/Users/josh/d/gitoxide/gitoxide-core/src/repository/diff.rs:19-31` for the
  current convenience tree-diff command.

### Gitoxide's CLI is not yet Git's CLI

The local `gix` binary describes itself as an unstable developer tool and
explicitly not a replacement for `git` in `/Users/josh/d/gitoxide/src/lib.rs:1-9`.

The current plumbing CLI has:

- `gix diff tree OLD_TREE NEW_TREE`.
- `gix diff file OLD_REVSPEC NEW_REVSPEC`.
- A `gix log` command that currently emits one-line commit summaries rather
  than full `git log -p` output.

The command definitions are in
`/Users/josh/d/gitoxide/src/plumbing/options/mod.rs:602-645`; dispatch is in
`/Users/josh/d/gitoxide/src/plumbing/main.rs:306-344`; the current log
implementation is in `/Users/josh/d/gitoxide/gitoxide-core/src/repository/log.rs:14-50`.

The gitoxide crate-status document is explicit about the current diff scope:

- Simple blob line diffs are implemented.
- Patch generation has substantial unchecked areas.
- Worktree hunks, perfect Git heuristic parity, and several patch details remain
  incomplete.

Those gaps are listed in `/Users/josh/d/gitoxide/crate-status.md:351-390`.

Therefore, integrating with gitoxide is promising for a new native command, but
it cannot immediately replace the current `git diff`/`git log -p` workflow with
byte-for-byte Git compatibility.

## Recommended Layering

The renderer should not depend directly on `gix`, `gix-object`, or `gix-diff`.
Instead, introduce a small renderer-facing event layer.

```text
gix / gix-diff
       │
       │ adapter translates gitoxide types
       ▼
diff-pretty render events
       │
       ├── existing text parser feeds events
       ├── gitoxide adapter feeds events
       └── future Git adapter could feed events
       │
       ▼
RenderedDocument
       │
       ├── native pager
       └── byte-compatible writer
```

This separates three concerns:

1. **Repository traversal:** revision parsing, tree lookup, object access,
   attributes, filters, rename tracking, and worktree state.
2. **Diff semantics:** tree changes, blob hunks, line kinds, and hunk ranges.
3. **Presentation:** word-diff pairing, ANSI styles, line numbers, hunk boxes,
   retained lines, and terminal drawing.

The current `render(&str) -> String` API can remain as a compatibility boundary.
Its parser would produce the same internal events that the gitoxide adapter
produces. That allows the existing Git-pager fixtures and the new gitoxide path
to share rendering tests.

## Proposed Renderer Event Model

The event model should be owned by `diff-pretty` or a small shared rendering
crate, not by `gix-diff`. `gix-diff` should remain a repository-agnostic diff
algorithm crate.

An initial model could be:

```rust
enum RenderEvent<'a> {
    CommitBegin(CommitMeta<'a>),
    CommitMessage(&'a [u8]),
    FileBegin(FileMeta<'a>),
    HunkBegin(HunkMeta),
    HunkLines(&'a [(LineKind, &'a [u8])]),
    Binary(BinaryMeta<'a>),
    Submodule(SubmoduleMeta<'a>),
    FileEnd,
    CommitEnd,
}
```

This is illustrative rather than a final API. Important properties:

- `LineKind` should be renderer-owned and map from `gix_diff::DiffLineKind`.
- `HunkMeta` should preserve old/new starts and lengths.
- `FileMeta` should carry old/new paths, modes, IDs, and rewrite information.
- Commit metadata should be separate from blob diff events.
- Binary and submodule output need explicit variants rather than string hacks.
- Borrowed payloads should be valid only for the call; the document owns what it
  needs afterward.
- Event errors should be typed and able to stop generation cleanly.

The renderer's event sink could be a trait:

```rust
trait RenderSink {
    type Error;

    fn event(&mut self, event: RenderEvent<'_>) -> Result<(), Self::Error>;
    fn finish(self) -> Result<RenderedDocument, Self::Error>;
}
```

The existing text parser and the gitoxide adapter would both implement the same
producer side, while `RenderedDocument` remains the consumer-facing result.

## Integration Options

### Option A: `diff-pretty` embeds gitoxide as a library

Add optional `gix`/`gix-diff` dependencies to `diff-pretty` and expose a new
command such as:

```sh
diff-pretty gix diff OLD NEW
diff-pretty gix show REV
```

The command would open the repository directly, resolve revisions, walk trees,
load blob resources, emit render events, and enter the native pager in the same
process.

Advantages:

- No gitoxide fork required.
- No Git child process or Git-to-pager pipe.
- Fastest route to validating the renderer event model.
- Existing native pager can be reused immediately.
- All code remains under this repository while the API is experimental.

Costs:

- Large dependency and compile-time increase if `gix` is enabled in the main
  binary.
- More repository behavior becomes our responsibility.
- `git diff` still invokes Git unless users adopt the new command or an alias.
- Gitoxide's current command-parity gaps remain visible.

This is the best prototype path.

### Option B: a new `gix-diff-pretty` crate in the gitoxide workspace

Create a renderer/adaptor crate beside `gix-diff`:

```text
gix-diff
    ├── tree and blob diff algorithms
    └── borrowed change/hunk callbacks

gix-diff-pretty
    ├── gix-diff → RenderEvent adapter
    ├── styles and line-number layout
    ├── retained document
    └── optional native pager

gitoxide CLI
    └── gix diff / gix log integration
```

This is the cleanest long-term ownership model if gitoxide maintainers are
interested. It avoids making the low-level `gix-diff` crate depend on terminal
UI code and lets other Rust applications consume the formatted document without
using the CLI.

The current `diff-pretty` renderer could either move into that crate or remain a
separate implementation while the event model stabilizes. Moving code too early
would make comparison with the current golden contract harder, so an adapter
prototype should come first.

### Option C: add a native pager to the existing `gix` CLI

The existing top-level command runner already passes `&mut dyn Write` output
handles through `prepare_and_run()` in `/Users/josh/d/gitoxide/src/shared.rs:53-65`.
That makes it straightforward to add a pager around serialized command output.

However, a `Write` wrapper would only reproduce the current external-pager
architecture inside a Rust process:

```text
gix diff → String/bytes → Write wrapper → parser → native pager
```

It removes a process boundary but not the serialized representation or reparsing.
It is useful as a compatibility step, not the event integration we actually want.

### Option D: make gitoxide a `git` replacement

This is the largest option and should not be the starting goal. It requires
implementing enough of Git's command and output behavior that users can replace
`git diff`, `git show`, and `git log -p` without surprises.

The existing gitoxide status explicitly shows that diff patch details and exact
heuristic parity are not complete. Native pager integration should not be held
hostage by full Git compatibility, nor should it claim compatibility before the
underlying repository workflows exist.

## Mapping Gitoxide APIs to the Renderer

### Tree changes

Use the callback-oriented tree diff APIs rather than the convenience method that
returns a `Vec`:

1. Resolve old and new trees using `gix::Repository`.
2. Configure path tracking and rewrite options.
3. Run `gix_diff::tree()` or `gix_diff::tree_with_rewrites()` with a delegate.
4. Translate `Addition`, `Deletion`, `Modification`, and `Rewrite` into
   `FileBegin` metadata.
5. For file content changes, prepare the corresponding blob resources.

The tree event gives paths, modes, object IDs, and rename/copy relationships,
which is more information than the current parser can recover reliably from
serialized headers.

### Blob resources and filters

For a repository-faithful diff, the adapter should use gitoxide's resource
pipeline instead of reading raw objects blindly. The local
`gix-diff::blob::platform::Options` and `Pipeline` APIs handle resource
conversion, attributes, textconv, binary detection, and large-file policy.

The relevant high-level entry point is `Repository::diff_resource_cache()`;
the current file-diff implementation demonstrates the setup in
`/Users/josh/d/gitoxide/gitoxide-core/src/repository/diff.rs:155-215`.

This is one of the main reasons to integrate at the gitoxide API layer rather
than reimplementing repository access in `diff-pretty`.

### Hunk events

For each modified text blob:

1. Obtain the diff algorithm and interned input.
2. Run `gix_diff::blob::diff_with_slider_heuristics()` or the configured
   algorithm.
3. Wrap it in `gix_diff::blob::UnifiedDiff`.
4. Provide a `ConsumeHunk` delegate that forwards `HunkHeader` and typed lines
   into the renderer.

The delegate receives all lines in a hunk as borrowed byte slices. That is a
good boundary for the current word-diff inference, which already operates on
groups of removed and added lines.

### Commit history

`gix` exposes revision walking and commit objects, so a future `gix log -p`
implementation could:

1. Parse a revision specification.
2. Walk commits in the requested order.
3. Read author, committer, dates, message, decorations, and parent IDs.
4. Diff each commit tree against its selected parent tree.
5. Emit commit and file events into the same renderer.

The current `gitoxide-core` log command only prints a short ID and first message
line. Full patch history is therefore a separate gitoxide feature, not merely a
pager hookup.

## Pager Ownership

The existing native pager in `diff-pretty` can be reused in Option A, but a
gitoxide workspace contribution should avoid making `gix-diff` depend on this
application crate.

Possible ownership models:

### Extract a reusable pager crate

Move the terminal session and `PagerDocument` into a small crate with an API
such as:

```rust
pub fn page(document: RenderedDocument) -> Result<(), Error>;
```

The crate could use `rustix` for Unix termios and a separate Windows backend.
Gitoxide already has optional terminal-related dependencies for progress
rendering, but those should not be pulled into low-level diff crates merely to
support paging.

### Keep pager and rendering separate

Put `RenderedDocument` and event rendering in a reusable no-terminal crate, then
keep the native pager as an optional CLI frontend. This is more composable:

```text
gix-diff-pretty-core  → document and serialization
gix-diff-pretty-pager → terminal input/output
gix CLI               → chooses stdout or pager
```

This is preferable if gitoxide wants machine-readable or GUI consumers of the
same diff events.

### Keep the current pager temporarily

For the first prototype, keep `diff-pretty`'s pager where it is and prove the
event adapter with a `RenderedDocument` sink. Pager extraction can follow once
the renderer boundary is stable.

## Cancellation and Interactive Generation

Gitoxide's callback APIs already provide better cancellation primitives than
Git's C pager path. Tree delegates can return `ControlFlow::Break`, and hunk
consumption stops after the first error.

There are still two possible modes:

### Eager document generation

1. Walk trees and blobs.
2. Build `RenderedDocument`.
3. Enter the native pager.

This is the safest first version. It avoids holding repository traversal state
while the user scrolls and makes `q` a local pager concern.

### Interleaved generation and paging

The pager could draw the first screen while gitoxide continues walking the
repository. This is more feasible than in Git because all components are Rust,
but it still needs a producer/consumer design, buffering, and a cancellation
contract.

The existing callbacks are synchronous and borrow their inputs. A streaming
pager would need either:

- A queue of owned render events.
- A thread-local cooperative event loop.
- A retained document that can be appended while the pager reads it.

The recommended first version is eager generation. The current benchmarks show
that rendering/indexing is already measurable but small compared with the full
renderer cost; concurrency should be justified by first-paint measurements.

## Configuration and CLI Shape

For Option A, a direct subcommand is clearer than trying to impersonate Git's
`core.pager`:

```sh
diff-pretty gix diff HEAD~1 HEAD
diff-pretty gix show HEAD
diff-pretty gix log-p
```

For Option B, the natural shape is likely:

```sh
gix diff --pager=native OLD NEW
gix show --pager=native REV
gix log --patch --pager=native
```

The exact CLI should follow gitoxide's existing command conventions rather than
copying every Git flag immediately.

The output policy should be explicit:

- Interactive stdout: build a document and page natively.
- Non-interactive stdout: serialize exact output to the provided writer.
- `--no-pager`: bypass terminal setup.
- Explicit external pager: remain available where gitoxide supports it.
- Terminal size: query at pager startup; resize handling can remain deferred.

## Build and Dependency Strategy

### Standalone prototype

Add optional gitoxide dependencies to `diff-pretty`:

```toml
[features]
gitoxide = ["dep:gix", "dep:gix-diff"]
```

This keeps the current lightweight stdin renderer as the default. The tradeoff
is that enabling the feature brings in a large repository implementation and
substantial compile-time cost.

### Workspace crate

Add a new crate to the gitoxide workspace that depends on `gix`, `gix-diff`, and
the rendering core. This avoids making the `gix-diff` algorithm crate know about
terminal presentation and is the cleanest contribution boundary.

### Shared renderer crate

If the event model proves useful beyond this project, extract it into a small
crate with no repository dependency. `diff-pretty`, gitoxide, and future GUI or
editor integrations could then implement adapters independently.

Do not add gitoxide as an unconditional dependency to the current binary until
the prototype demonstrates a real workflow and acceptable build cost.

## Compatibility and Feature Gaps

The first gitoxide integration should explicitly state which behavior it does
and does not promise.

### Likely straightforward

- Two-tree diffs.
- Blob line diffs.
- Hunk line numbers and context.
- Additions, deletions, and modifications.
- Native paging in the same process.
- ANSI styling generated entirely by the Rust renderer.

### Requires adapter work

- Rename and copy metadata.
- Attribute-aware textconv and filters.
- Binary diff presentation.
- Submodules.
- Worktree versus index versus tree comparisons.
- Pathspec filtering.
- Git-compatible diff algorithms and heuristics.

### Larger gitoxide feature work

- Full `log -p` history.
- Decorations and commit metadata parity.
- Merge and combined diffs.
- Exact Git patch serialization.
- All `git diff` flags and configuration semantics.
- Git's complete object/revision behavior.

The renderer should use feature flags or command-level capability checks rather
than silently pretending unsupported cases are equivalent to Git.

## Measurement Plan

Compare three paths on the same repositories:

1. `git diff | diff-pretty` with the native pager.
2. A standalone `diff-pretty gix diff` command.
3. A workspace-integrated `gix diff --pager=native` command.

Measure separately:

- Repository open and revision resolution.
- Tree traversal and rename tracking.
- Blob/filter preparation.
- Diff algorithm time.
- Event-to-render time.
- Document setup allocation bytes and counts.
- Time to first painted viewport.
- Peak RSS.
- Total wall time.
- Non-interactive serialized output equality.

The goal is not only fewer processes. Gitoxide may change the diff algorithm,
object cache behavior, filter handling, or rename implementation, so the
comparison must identify which layer caused any improvement or regression.

## Staged Implementation

### Stage 0: API spike in `diff-pretty`

Build a feature-gated `gix` command for two committed trees. Use existing
`gix-diff` callbacks to populate the current renderer's event model, then use
the existing native pager. Do not promise Git output parity yet.

Acceptance criteria:

- No Git child process.
- No Git-to-pager pipe.
- A two-tree repository diff renders through the native pager.
- Hunk line numbers and add/remove/context styling are correct.
- Non-interactive output is deterministic.

### Stage 1: reusable gitoxide adapter crate

Move the adapter into a workspace-friendly crate or a standalone core module.
Keep `gix-diff` and the pager independent of each other.

Add tests at the event boundary rather than testing only final ANSI bytes:

- Tree change mapping.
- Hunk header mapping.
- Binary and incomplete-line behavior.
- Borrowed byte lifetime and ownership.
- Early `ControlFlow::Break` behavior.

### Stage 2: worktree and index comparisons

Use gitoxide's resource cache, attributes, filters, and status/diff APIs to cover
the workflows users expect from `git diff`.

### Stage 3: commit history

Implement commit event production and parent-tree diffs for `gix log -p`-like
output. Only then compare against the existing `fixtures/*.patch` corpus.

### Stage 4: document and pager extraction

If the event path lowers allocations or improves first paint, extract the
retained document and pager into reusable crates. If not, keep the simpler
serialized compatibility path.

## Risks

### API evolution

Gitoxide's own documentation describes the `gix` CLI and crate as unstable. A
contribution should target public `gix-diff` traits and add missing stable
delegates rather than depending on private fields or the current `gitoxide-core`
command layout.

### Diff parity

The `gix-diff` crate-status document still tracks patch details and perfect
heuristic parity as incomplete. A prettier renderer cannot compensate for a
different diff algorithm if byte-identical Git output is the contract.

### Memory ownership

Borrowed hunk callbacks are excellent for avoiding intermediate strings, but a
retained pager document must copy or own anything it needs after the callback.
The event path reduces serialization and reparsing; it does not guarantee zero
copy.

### Repository lifetime

Gitoxide repository and tree handles carry lifetimes and caches. The renderer
should not retain repository-backed objects in the document. Convert them to
small owned metadata values and keep object access in the producer.

### Terminal and progress output

The `gix` CLI already has progress-rendering infrastructure and an output helper
that accepts `&mut dyn Write`. Native paging must decide whether progress is
disabled, moved to stderr, or coordinated with the pager. The first version
should keep progress and the diff viewport separate.

### Command scope

Adding a `gix` diff renderer does not change `git`'s `core.pager` behavior. Users
must invoke `gix`/`ein` or configure an alias. Replacing the stock `git` command
still requires a wrapper, a Git fork, or eventual command parity.

## Recommendation

Gitoxide is the most promising place to pursue the deeper idea. The existing
Rust APIs already provide:

- Repository-native object and revision access.
- Tree change delegates.
- Rename-aware change types.
- Borrowed unified hunk callbacks.
- Early traversal cancellation.
- A workspace where a presentation crate can be added without FFI.

Start with a feature-gated `diff-pretty gix diff` prototype for two committed
trees. Feed `gix-diff` events into a renderer-owned event sink and reuse the
native pager. Do not begin with full `git log -p` parity or by wrapping the
existing `Write` output path; those approaches either expand scope too quickly
or preserve the serialization boundary we want to remove.

