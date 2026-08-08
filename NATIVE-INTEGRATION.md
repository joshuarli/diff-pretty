# Native Git Integration

This document records the native Git integration contract. Stage 2 is now
implemented for the local Git 2.55 fork: `git diff` can opt into an in-process
Rust renderer through `builtin:diff-pretty`, while non-interactive output and
unsupported commands retain Git's ordinary path.

The remaining stages—commit/log events and retained-document optimization—are
intentionally not part of this first usable milestone.

## Summary

The current process topology is:

```text
git diff / git show / git log -p
            │
            │ serialized diff bytes through a pipe
            ▼
diff-pretty
            │
            │ native terminal drawing
            ▼
terminal
```

The native pager removed the second half of the old pipeline:

```text
diff-pretty → less → terminal
```

There is still one process boundary and one pipe between Git and
`diff-pretty`. `core.pager` cannot remove that boundary by configuration alone:
Git treats the configured value as a shell command and starts it as a child
process.

The native version is now available in the companion Git fork: Git emits its
existing diff-symbol stream through an opaque C ABI, Rust builds a retained
document, and `scrl` drives the terminal without a Git-to-child pipe. The
external process topology remains the compatibility path for stock Git and
unsupported commands.

## Current Git Boundary

The integrated checkout is Git 2.55.0 under
`/Users/josh/d/git-minimal-musl-static/src/git-2.55.0`. The relevant path is:

1. `git diff` calls `setup_diff_pager()` before generating the diff in
   `builtin/diff.c`.
2. `setup_diff_pager()` decides whether paging is allowed and calls
   `setup_pager()` in `diff.c:setup_diff_pager()`.
3. `git_pager()` resolves `GIT_PAGER`, `core.pager`, `PAGER`, and the default
   pager in `pager.c:git_pager()`.
4. `prepare_pager_args()` stores the configured command, marks it for shell
   execution, and starts a child process in `pager.c:setup_pager()`.
5. Git duplicates the child's pipe onto file descriptor 1. Later Git output is
   ordinary writes to that redirected stdout.

Git also sets `COLUMNS` before redirecting stdout and exports
`GIT_PAGER_IN_USE`. Those are useful compatibility details for an external
pager, but they do not provide a structured integration API.

`git log -p` follows a similar setup path through Git's log builtin, while
commit headers and other log output
are produced outside the ordinary diff-line path. This matters because an
integration that handles only diff hunks will not reproduce `git log -p`.

## Goals

An eventual native integration should aim to:

- Remove the Git → `diff-pretty` process and pipe for supported commands.
- Preserve Git's existing command-line and pager semantics.
- Preserve byte-identical output for non-interactive stdout.
- Keep `--no-pager`, `--paginate`, `GIT_PAGER`, `PAGER`, and command-specific
  pager configuration understandable.
- Allow the Rust side to build a retained document without reparsing a complete
  serialized patch.
- Keep terminal ownership, raw mode, alternate-screen cleanup, and pager input
  in the Rust side.
- Retain an external-pager fallback for stock Git and unsupported commands.

The integration should not initially attempt to replace Git's diff algorithms,
object traversal, rename detection, merge handling, or repository access. Git
should remain responsible for deciding *what changed*; `diff-pretty` should
remain responsible for presentation.

## Non-Goals

The first version should not try to:

- Become a general Git UI.
- Reimplement Git's diff generation in Rust.
- Make the Rust renderer depend on Git's private C structs directly.
- Support every Git command that can use a pager.
- Add live resize, search, mouse input, or other pager features as part of the
  integration work.
- Make the event ABI an upstream Git API before the event model has evidence
  behind it.

## Proposed Architecture

### 1. A Git-side adapter

Add an optional Git integration layer that owns an opaque Rust session:

```text
Git command initialization
        │
        ├── decide native vs external pager
        ├── capture terminal/input/output state
        └── create diff-pretty session

Git diff/log machinery
        │
        ├── commit events
        ├── file-pair events
        ├── hunk events
        ├── content events
        └── binary/submodule/separator events
        │
        ▼
Rust adapter → RenderedDocument → native pager
```

The C side should expose only a small, versioned C ABI. It should not expose
Rust lifetimes, `String`, `Vec`, or Rust trait objects across the boundary.
Conceptually, the API would look like:

```c
struct diff_pretty_session;

struct diff_pretty_config {
    uint32_t version;
    uint32_t size;
    unsigned use_color;
    unsigned word_diff;
};

struct diff_pretty_session *diff_pretty_begin(
    const struct diff_pretty_config *config,
    int output_fd,
    int input_fd);

int diff_pretty_commit_begin(
    struct diff_pretty_session *, const struct diff_pretty_commit *);
int diff_pretty_file_begin(
    struct diff_pretty_session *, const struct diff_pretty_file *);
int diff_pretty_hunk(
    struct diff_pretty_session *, const struct diff_pretty_hunk *);
int diff_pretty_line(
    struct diff_pretty_session *, const struct diff_pretty_line *);
int diff_pretty_file_end(struct diff_pretty_session *);
int diff_pretty_commit_end(struct diff_pretty_session *);
int diff_pretty_finish(struct diff_pretty_session *);
void diff_pretty_abort(struct diff_pretty_session *);
```

The exact API should wait until the event model is prototyped. The important
properties are explicit versioning, sizes on extensible structs, integer error
returns, and an opaque session handle.

### 2. A semantic event stream

Git's internal `DIFF_SYMBOL_*` enum is a useful source of inspiration, but it
should not become the public ABI. It is private implementation detail and can
change between Git versions.

The adapter should translate Git's internal events into a smaller stable model.
Potential events include:

#### Session events

- `SessionBegin`: command kind, terminal mode, color mode, width, and feature
  flags.
- `SessionEnd`: successful completion.
- `SessionAbort`: Git or pager cancellation.

#### Commit events

- `CommitBegin`: object ID and whether this is a synthetic or zero commit.
- `CommitHeader`: author, dates, subject, decorations, and raw header fields
  needed by the current renderer.
- `CommitMessageLine`: message lines with their original bytes.
- `CommitEnd`.

#### File events

- `FileBegin`: old/new paths, modes, object IDs, rename/copy status, and
  submodule/binary flags.
- `FileHeader`: raw or semantic metadata lines that Git currently emits.
- `FileEnd`.

#### Hunk and content events

- `HunkBegin`: old/new start/count and function-context fragment.
- `ContentLine`: context, addition, deletion, or incomplete-line marker, with
  line bytes and relevant whitespace/error flags.
- `BinaryDiff`: binary header/body/footer data.
- `SubmoduleOutput`: submodule status or commit summary.
- `Separator`: blank lines and file/commit boundaries where they are part of
  the visible contract.

The event payload should preserve enough information for `diff-pretty` to make
its own rendering decisions. Passing only Git's already-colorized text would
remove some reparsing but would not be a true semantic integration.

### 3. Retained rendering on the Rust side

The Rust side should eventually replace the current
`render(&str) -> String`-first path with an internal retained document:

```text
Git events
   │
   ▼
RenderedDocument
   ├── commit metadata
   ├── file metadata
   ├── hunk metadata
   └── styled logical lines / spans
        │
        ├── native pager viewport
        └── byte-compatible stdout writer
```

The public `render(&str) -> String` API can remain as a compatibility layer for
golden tests and external users. It could be implemented by feeding parsed
input into the same document model and serializing the final document.

The document should own the rendered/styled data it needs, but it should not
blindly duplicate every intermediate form. The design needs measurements for:

- A vector of owned rendered lines.
- Lines with style spans and borrowed source slices.
- A compact event/metadata representation rendered on demand.
- A spillable temporary file or memory-mapped backing store for unusually large
  output.

The existing `PagerDocument` line-start index is a useful first measurement
boundary, not the final architecture.

## Where to Hook Git

### Diff body hook

`struct diff_options` contains a `FILE *file`, several callback fields, and an
internal emitted-symbol buffer in `/Users/josh/d/git/diff.h:375-395`.

The central internal function `/Users/josh/d/git/diff.c:1633-1643`,
`emit_diff_symbol()`, either appends a semantic symbol to Git's internal buffer
or writes it through the existing output path. This is the most promising hook
for a diff-body adapter because it sees additions, deletions, context lines,
hunk headers, binary output, and other diff symbols before they become a final
serialized stream.

The adapter should sit beside this path, not replace Git's existing output
behavior globally. A native session would receive events; ordinary output and
external pagers would continue through `FILE *file` unchanged.

### Commit and log hook

`git log -p` emits commit headers and message metadata through log-tree code
outside the diff-symbol stream. A complete integration therefore needs a
second adapter around the log output path.

The first supported command could be `git diff`, followed by `git show`, and
then `git log -p`. Treating `git log -p` as a separate milestone is safer than
pretending the diff hook covers it.

### Pager-selection hook

The current pager selection in `pager.c` could recognize a reserved value such
as:

```ini
core.pager = builtin:diff-pretty
```

That value would select the in-process adapter rather than calling
`prepare_pager_args()` and `start_command()`.

Alternative configuration designs include a dedicated setting such as
`core.diffPretty = native`, or a build-time default. A reserved pager value is
closer to Git's existing mental model, but it must be documented as a Git-fork
extension and must never make stock Git fail confusingly.

The native path should still respect:

- `--no-pager` and commands that explicitly disable paging.
- `--paginate` and command-specific pager policy.
- Non-tty stdout, where exact bytes should be written without entering the
  interactive pager.
- `GIT_PAGER`, `PAGER`, and `core.pager` precedence when the native value is not
  selected.

## Event Lifetime and Copying

Git's diff callback arguments are generally borrowed for the duration of a
callback. Rust cannot retain those pointers after the callback returns unless
the adapter copies the bytes or Git gives the event stream an explicit lifetime
and ownership contract.

The safe initial rule should be:

```text
Git owns source/event memory during callback
Rust copies only data needed by RenderedDocument
Rust owns all data needed after the callback returns
```

This still removes the full serialized patch and the Git-to-process pipe. It
does not magically make all copies disappear. A later optimization can use
shared buffers or explicit ownership transfer once measurements justify the
complexity.

## Pager Lifecycle and Cancellation

The current external pager gets a natural cancellation mechanism: when the
user quits, the child exits and Git eventually observes a broken pipe or child
status.

An in-process pager has no pipe to break. That creates an important design
choice:

### Eager generation, then interactive paging

1. Git emits all events into `RenderedDocument`.
2. Rust enters the native pager.
3. `q` only exits the pager; Git's diff generation is already complete.

This is the safest first implementation. It avoids making Git's diff loops
cancellation-aware, at the cost of waiting for the entire document before the
first screen.

### Interleaved generation and paging

Git emits events while the native pager displays the document. This could
improve first-paint latency and reduce peak buffering, but it requires:

- A producer/consumer design inside one process.
- A thread or cooperative event loop around Git's non-thread-safe machinery.
- Backpressure when the user stops scrolling.
- A cancellation flag checked through Git's diff and log loops.
- Defined behavior when `q` happens during object traversal or text conversion.

This should not be the first native integration target.

### Error propagation

Every event callback needs an integer result. Rust should store a detailed error
inside the opaque session, return a small C-compatible failure code, and let the
Git adapter convert it into a Git diagnostic.

Pager quit should be a distinct non-error result from terminal failure, render
failure, or Git traversal failure. The native pager should restore raw mode and
the alternate screen before returning any of them.

## Build and Distribution Options

### Private Git fork with a static Rust library

Build `diff-pretty` as a `staticlib` or C-compatible static artifact and link it
into Git. Git's Makefile would need an optional target that invokes Cargo and
links the resulting library.

Advantages:

- No runtime discovery or dynamic-loader policy.
- One Git binary contains the integration.
- Calls can be direct and fast.

Costs:

- Git's C build becomes dependent on a Rust toolchain for this feature.
- Every supported platform needs linker and packaging work.
- Rebasing the Git fork becomes ongoing maintenance.
- The embedded Rust code must respect Git's build flags, allocators, and panic
  behavior.

### Dynamic library/plugin

Git could load a shared library at runtime, but this has worse portability and
deployment characteristics. There is no existing standard Git pager plugin
ABI, so this would be a new loader, discovery rule, security boundary, and
versioning problem.

### Separate `git-diff-pretty` builtin

Adding a `git-diff-pretty` executable or a new builtin command could provide a
tighter user experience, but it would not automatically replace `git diff`.
The normal `git diff` and `git log -p` paths would still need to be patched to
select it. This is useful as a prototype command, not a complete integration.

## Staged Experiment

The lowest-risk research sequence is:

### Stage 0: preserve the existing external path

Keep stock Git plus `core.pager=diff-pretty` as the compatibility baseline.
Measure:

- Git-to-renderer process startup.
- First-paint latency.
- Total wall time.
- Peak RSS.
- Allocations in Git and `diff-pretty` separately.

### Stage 1: add a private `builtin:diff-pretty` selector

Patch `pager.c` to recognize the selector and route only `git diff` to a
minimal adapter. Keep all other commands and configurations on the existing
pager path.

The first adapter can receive serialized output through an in-process capture
mechanism if that is easier than beginning with semantic callbacks. This stage
would validate Git build/link integration and lifecycle behavior, but it is not
the final memory design.

### Stage 2: diff-symbol events (implemented)

Add a versioned adapter around `emit_diff_symbol()` and support `git diff` in
the first milestone; keep `git show` on the existing path until commit events
are modeled. Compare:

- Event-to-document allocations.
- Output equality against current fixtures.
- First-paint latency.
- Behavior for binary, rename, submodule, and incomplete-line output.

The implementation uses `ffi/` for the opaque C ABI and maps Git's private
`enum diff_symbol` at
`git-minimal-musl-static/src/git-2.55.0/diff.c`. Explicit numeric assertions
make a Git enum reorder fail at compile time. The Rust side buffers one file
section before feeding the existing renderer, preserving its frozen output
behavior without a Git-side textual capture buffer.

### Stage 3: commit/log events

Add commit header, message, decoration, and separator events. Enable `git log -p`
only after its output is covered by the existing golden corpus.

### Stage 4: retained-document optimization

Only after the event model is stable, replace the current full ANSI `String`
with retained styled lines/spans and decide whether an in-memory or spillable
backing store is appropriate.

## Risks

### Git internal API stability

The most convenient hooks are private Git implementation details. Rebases can
change enum values, callback order, struct layout, and output timing. The
adapter must translate Git internals into its own versioned event model rather
than exposing Git structs to Rust.

### Output compatibility

The current project has byte-for-byte golden contracts. An event adapter must
cover all output that currently arrives on stdin, including raw passthrough
metadata and ANSI color sequences. A missing event can silently change the
rendered result.

### Command coverage

`git diff`, `git show`, and `git log -p` do not share exactly the same output
path. Merge diffs, submodules, external diff drivers, text conversions,
`range-diff`, and other commands need explicit decisions.

### Terminal ownership

Git currently owns pager process setup and signal cleanup. The Rust side would
own terminal mode and screen drawing, while Git would own command lifecycle and
repository traversal. The boundary must make cleanup unconditional on normal
quit, pager error, Git error, and signal paths.

### Early quit

Without a broken pipe, an in-process pager cannot stop Git's work unless the
event path is cancellation-aware. Eager generation avoids this problem and is
the recommended first milestone.

### Build complexity

An embedded Rust library changes Git's build, release, cross-compilation, and
distribution story. This is likely the largest non-code cost of the project.

## Recommendation

Native event integration is feasible as a private Git fork, but it is not a
small extension to `core.pager`. The right first experiment is a narrow,
opt-in `builtin:diff-pretty` mode for `git diff`, with eager event collection,
an opaque C ABI, and the existing native pager on the Rust side.

Do not start by trying to stream events concurrently with Git or by exposing
Git's private structs directly. First prove that an in-process diff-symbol
adapter improves wall time, first paint, or memory enough to justify supporting
a Git fork. Only then expand to commit/log events and a fully retained document.
