# diff-pretty's scrl integration

`diff-pretty` renders Git-style patches and embeds the standalone `scrl`
library to display the resulting UTF-8/ANSI text. The complete pager contract,
CLI, session API, input behavior, and terminal lifecycle are documented in
[`scrl/README.md`](scrl/README.md); this document records only the integration
boundary from the renderer's point of view.

## Ownership boundary

`diff-pretty` owns:

- Git metadata and patch parsing;
- hunk pairing and word-diff inference;
- diff-specific ANSI styles, decorations, line numbers, and fixed 80-column
  presentation;
- paging policy for its own CLI; and
- the adapter that turns rendered patch units into `scrl::ChunkSource`.

`scrl` owns the generic document, viewport, search, input decoding, source
backpressure, and terminal lifecycle. The live runner pulls only the initial
viewport and requests more rendered chunks for forward navigation, `End`, or
full-input search; it retains the loaded prefix rather than evicting it. It
does not know about Git, hunks, files, line-number styles, or the fixed width
of the renderer.

The dependency direction is one-way:

```text
diff-pretty renderer ────────┐
                             ├──> scrl library
diff-pretty binary ──────────┘
```

The binary is never invoked as a subprocess and no external pager or `$PAGER`
configuration is consulted.

## Embedded render path

For terminal paging, `src/source.rs` constructs `RunOptions` and calls
`scrl::run_source` with a diff-specific `ChunkSource`:

```text
patch stdin
  → diff-pretty parser and renderer
  → complete parser-safe ANSI chunks
  → scrl::run_source
  → scrl::Session and terminal adapter
```

`for_each_render_chunk` remains responsible for choosing safe boundaries. A
chunk is never split inside a diff file because word-diff pairing needs the
complete hunk/file context. `scrl` treats each chunk as generic text and never
infers or splits Git structures.

When paging is disabled, stdout is not a terminal, or the terminal cannot be
used, diff-pretty renders directly through `render_reader_to`. Direct output
does not enter an alternate screen and remains byte-for-byte compatible with
the checked-in golden fixtures.

## Embedded options

The embedded session deliberately uses the fixed diff presentation:

```rust
SessionOptions {
    title: "diff-pretty".into(),
    search_history: Vec::new(),
    wrap: false,
    follow: false,
    filter: None,
}
```

Search, highlighting, navigation, horizontal movement, help, and terminal
cleanup are inherited from `scrl`. Wrapping, follow mode, filtering, and file
operands are standalone pager features and are not enabled for rendered diffs
because they would change the fixed presentation or input contract.

## Invariants and validation

- `scrl` remains independent of the renderer and has no dependency on this
  package.
- The renderer's 80-column output is independent of terminal dimensions.
- Search operates on visible text, never serialized ANSI bytes or Git metadata.
- Retained, streaming, and incremental render paths remain byte-equivalent.
- `--paging=never` and direct-output fallback preserve existing behavior.
- Quitting or terminal failure restores raw mode and alternate-screen state.

Run the integration judges with:

```sh
cargo test --workspace --all-targets --release
make check
```

The standalone package's tests and focused redraw benchmarks live under
`scrl/`; see [`scrl/README.md`](scrl/README.md) for those commands.
