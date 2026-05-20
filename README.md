# Burger To Cow

This project tackles an intentionally lossy reverse-engineering problem. We
have:

* a plain-text **source template** processed by the
  [minijinja](https://docs.rs/minijinja) rust template engine, with full
  access to both the template file and the context map,
* the **expanded** (rendered) output,
* an **edited** version of the expanded output that some downstream tool has
  modified.

Given these three, we want to reconstruct what a patch to the *template*
would look like — i.e. distinguish edits that belong to the template from
edits that are really just new variable values.

This is the classic "hamburger back into a cow" problem and has no
generally-deterministic solution. But because we also control the
rendering step we can instrument it: that turns the general case into a
collection of well-understood heuristics that handle the common patterns
correctly and fall back safely to a human-decision block for the rest.

## Coverage at a glance

Use this table to gauge how much of your workload the tool will handle
autonomously. "Determined" means the reverse-diff is emitted with no
human intervention; "conflict" means the output is a
`<<<< diff decision needed >>>>` block that flags the ambiguity for
manual resolution; "conservative guess" means we emit a valid template
diff but the attribution between template and variable could be wrong in
theory.

### Determined correctly

| Scenario | Example |
|---|---|
| Unchanged render (no edits) | render == modified → empty diff |
| Pure variable value change | `{{ user }}` rendered as `Ada`, modified to `Ida` → empty diff |
| Static-text edit outside any loop/conditional | `Welcome.` → `Greetings.` in a header line |
| Static-text edit inside a rendered conditional branch | `{% if ok %}Hi{% endif %}` with `ok=true`, `Hi` → `Hello` |
| Same static edit applied in every loop iteration | `- {{ i }}` → `* {{ i }}` across all iterations |
| Mixed template + variable edits (both categories in one run) | template edit captured; variable edit silently dropped |
| Unicode in template or variable values | CJK, emoji, combining marks all round-trip |
| Variable value containing marker control bytes | sanitised; tracking invariant preserved |

### Surfaced as a conflict (safe, but needs a human)

| Scenario | Why |
|---|---|
| Different static edits across loop iterations | We can't tell which iteration "wins" as the template rule |
| Loop body varies per iteration (`{% if loop.first %}…`) | Later iterations don't match the first one's skeleton, fallback can't route them |
| Edit inside a conditional branch that **didn't** render | We have no rendered twin to map back from |
| Edit whose mapped template range clashes with another edit's range but with different replacement text | Overlapping replacements can't be merged |

### Conservative (reported as template edit — usually correct, sometimes a false positive)

| Scenario | What we do |
|---|---|
| Insert at the exact first/last character of a `{{ v }}` region | Treated as template edit; could also be "variable got a prefix/suffix" |
| Insert between two adjacent `{{ a }}{{ b }}` variables with no static separator | Treated as template edit |
| Variable value shares characters with surrounding static text (fragmented char-level diff) | Whatever char-level Myers decides — may split one logical edit across seams |

### Out of scope

| Scenario | Notes |
|---|---|
| HTML-auto-escape with `Value::from_safe_string` carrying raw marker bytes | Explicit passthrough of unescaped content; documented in `engine` module |
| Templates that emit our marker bytes (`U+001E`/`U+001F`) as literal static text | Would confuse the skeleton parser; vanishingly rare in real content |
| Reconstructing which *variable* a changed value belongs to by fingerprinting | Not attempted — we only classify edits, we don't re-attribute values |

## Algorithm — "Skeleton + LCS + Loop Fallback"

1. **Shadow rendering**. `Tracker` installs a custom
   [`set_formatter`](https://docs.rs/minijinja/latest/minijinja/struct.Environment.html#method.set_formatter)
   on minijinja that wraps every variable emission in a pair of ASCII
   information-separator control characters (`U+001E` start, `U+001F`
   end). The `TrackedRender` keeps both the visible output (markers
   stripped) and the marker-annotated *tracked* stream. Variable values
   are sanitised against marker-byte injection so the invariant
   `#start == #end == #variables` always holds.
2. **Skeleton distillation**. For the tracked render, every
   variable emission collapses to a single `\0` sentinel, producing
   `r_skel`. For the source template, a lightweight tokenizer collapses
   every `{{…}}`, `{%…%}`, and `{#…#}` cluster into `\0`, producing
   `t_skel`.
3. **LCS alignment**. We run a Myers LCS pass (via the `similar` crate)
   over the two skeletons. Equal regions give us a direct
   render-char → template-char coordinate map.
4. **Loop-iteration fallback**. In a `{% for %}` loop, the template side
   has the loop body once while the rendered side has it N times — so
   iterations 2..N come back as LCS "insertions" (unmapped). Before
   walking the actual edits we post-process the map: each maximal
   unmapped run is matched against any identically-shaped *mapped* run in
   the render skeleton, and if we find one we copy its template indices
   into the unmapped run. Consistent edits across every iteration then
   consolidate to one template-level change; inconsistent edits surface
   as a conflict.
5. **Reverse-diffing**. We diff the *modified* text against the *pure*
   render and classify each hunk:
   * hunks that fall entirely inside a tracked variable region are
     dropped (pure data edit),
   * hunks that map to a single template range are converted into
     template-space replacements,
   * hunks that cannot be mapped, or two hunks that map to the *same*
     template range with disagreeing replacement text, become a
     `<<<< diff decision needed >>>>` conflict block for a human to
     resolve.

## Assumptions and Trade-offs

1. **Skeleton uniqueness.** Alignment assumes loop bodies render
   identically across iterations modulo variable values. Iteration-
   specific static text (e.g. `{% if loop.first %}` used to change
   punctuation) won't match the fallback pattern and stays unmapped —
   the safe outcome is "conflict block", not a wrong guess.
2. **Boundary inserts are conservative.** An insert sitting exactly at
   a variable's boundary (the first or last char of `{{ v }}`) is
   classified as a template edit, even though extending the value is
   also plausible. This reliably catches new static text but means
   "the user typed a suffix onto a variable value" may surface as a
   spurious template diff.
3. **Adjacent variables without a separator.** `{{ a }}{{ b }}` with
   an insert between them cannot be attributed to either variable with
   confidence; we call it a template edit.
4. **Char-level diff granularity.** We diff at the character level, so
   variable values that share characters with surrounding static text
   can produce fragmented Myers alignments. Downstream, these may be
   detected as conflicts even though a line-level interpretation would
   be unambiguous. This is deterministic but may be a surprise.
5. **Auto-escape interaction.** The formatter sanitises marker bytes
   from variable values in the `AutoEscape::None` path. For HTML and
   JSON auto-escape the escape itself already neutralises the marker
   bytes, so normal values are safe; an `HTML` mode combined with a
   `Value::from_safe_string` whose raw bytes include a marker is the
   residual risk and is flagged in the engine-module docs.

## Usage

The workspace ships two crates: `burgertocow-lib` (the library, imported
as `burgertocow`) and the `burgertocow` CLI.

### Library

The rendering surface mirrors `minijinja::Environment` so swapping
`Tracker` in for a plain `Environment` is a local change:

```rust
use burgertocow::{generate_diff, Tracker};

let mut tracker = Tracker::new();
tracker.add_template("greet", "Hello {{ user }}!\nWelcome.").unwrap();

let ctx = serde_json::json!({ "user": "Arthur" });
let tracked = tracker.render("greet", &ctx).unwrap();

// `tracked.output()` is what you send downstream (same as plain minijinja).
// `tracked.tracked()` retains the variable-boundary markers.
let modified = "Hello Zaphod!\nWelcome, friend.";
let diff = generate_diff("Hello {{ user }}!\nWelcome.", &tracked, modified);
```

For full control (filters, globals, loaders, autoescape, ...) reach through
`tracker.env_mut()` — it's the raw `minijinja::Environment`. Do not call
`set_formatter` on it; that would replace the tracking formatter.

### CLI

```bash
# Render with markers-stripped output:
burgertocow render --template tests/fixtures/simple.md --data tests/fixtures/simple_data.json --out out.md

# Reverse-diff the modifications:
burgertocow diff --template tests/fixtures/simple.md --data tests/fixtures/simple_data.json --modified mod.md
```

## Testing

The project has three layers of tests:

* **Unit tests** inside each module (`engine`, `parser`, `diff`). These
  lock in each stage's invariants (marker balance, skeleton shape, map
  arithmetic, conflict formatting).
* **Fixture-based integration tests** (`tests/integration.rs` +
  `tests/fixtures/`) exercising the full pipeline on realistic files:
  plain templates, pure-variable edits, consistent loop edits
  (heuristic win), inconsistent loop edits (conflict), conditionals,
  Unicode.
* **Property tests** (`tests/proptests.rs`) using `proptest` for
  invariants: unchanged render → empty diff, clean variable swap → empty
  diff, static-text edit → non-empty diff, `generate_diff` never panics,
  markers stay balanced under hostile values.

Run the same checks CI runs locally:

```bash
bin/check         # fmt + clippy + tests
bin/check-fmt     # rustfmt --check
bin/check-lint    # clippy -D warnings
bin/check-tests   # nextest (or cargo test)
```

To install the pre-commit hook:

```bash
ln -sf ../../scripts/pre-commit .git/hooks/pre-commit
```

## License

MIT — see [`LICENSE`](LICENSE).
