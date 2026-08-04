# TODO / known gaps

Tracked here so they're not lost. The README's "Coverage & known gaps" section
mirrors this list. All other git-pager behavior (`git diff`, `git show`,
`git log -p`) is locked in as byte-for-byte against the oracle.

1. **Extreme minus/plus imbalance in word-diff pairing.**
   In a giant deletion/insertion hunk (e.g. a 76-line removal vs a 4-line
   addition), our greedy word-diff pairing assigns the few homologs to
   different lines than delta, so a small number of lines (≈0.06% of a 19K-line
   `git log`) differ in emphasis — some we over-highlight, some we under-
   highlight, because the plus lines get consumed by different minus lines.
   Regression those: low-effort repro is the `@@ -73,82 +67,10 @@` hunk in a
   `git log -p` of `~/d/delta`. Likely needs to match delta's greedy pairing
   order for imbalanced runs.

2. _(done)_ Plain unified diffs without a `diff --git` header (`diff -u` /
   `git diff --no-index` in that form) are now rendered, not passed through.

3. **Other delta modes not implemented.** The following explicit delta modes
   have inputs without the usual `diff --git` header and are out of scope; they
   would not match delta:
   - `git blame` (per-line blame output).
   - `git grep` / ripgrep output handling.
   - merge / combined diffs (`git log -p` of merge commits).
   - submodule logs / diff.
