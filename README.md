# Burger To Cow

This project is about an interesting engineering challenge. Consider that we have a source plain text template file that is processed via the minijinja rust template engine, and have access to both the source file and the context map for injection, and the generated, expanded file.
Now, some other tool changes the expanded file, we want to reconstruct what a diff to the template would be, that means, telling a part what are changes to the template file vs variables .

Of course, this has no deterministic and generalizable solution. However given the specifics (being in possession of the source text, the value hash and able to instrument the template engine)  we can arrive at heuristics/ algos that , even if not for 100% of the cases , make the correct diff, leaving a minor percentage of occurrences as ambiguous.

## Ideas and Solution

**The "Skeleton and Alignment" Algorithm**

1. **Shadow Rendering**: The `Tracker` utilizes minijinja's `set_formatter` API to wrap every variable output into invisible Unicode bounds (`<U+001e>` and `<U+001f>`). By capturing this `TrackedRender`, we construct a 100% accurate map dividing the output byte-array into `Variable` vs `Template` regions.
2. **Skeleton Distillation**:
    - For the Output, we strip out all variables replacing them with a sentinel value `\0`, creating a `RenderSkeleton`.
    - For the `SourceTemplate`, we use a lightweight minijinja tokenizer to mask all `{{}}`, `{% %}`, and `{# #}` clusters as `\0`, creating a `TemplateSkeleton`.
3. **LCS Mapping Alignment**: Since both Skeletons now purely represent the unbroken *static boilerplate text*, we can align them perfectly utilizing the Longest Common Subsequence logic (powered by the `similar` crate). This gives us a rigorous, mathematical coordinate mapping from the rendered output's static text directly to the source template lines.
4. **Reverse Engineering the Diff**: We diff the `ModifiedText` against the pure original render. 
   - If the user only modified text bounded by our Variable Regions, we intercept the event and declare a pure Data overriding. No Template Diff is emitted.
   - If the user modifies standard text, we map their modified cluster back using our alignment logic to the `SourceTemplate`.

## Assumptions and Trade-offs

1. **Deterministic Alignment Assumption**: We assume the target user doesn't inject variables that perfectly match standard boilerplate text layout to trick the diff engine.
2. **Ambiguity Condition Breakdown**:
    - In templates using control flow operators like `{% for %}`, a single line of static text is expanded into multiple lines.
    - Our LCS sequence maps `RenderSkeleton` array nodes mathematically backwards. When evaluating loop multiplicities, `similar` aligns exclusively to the *first iteration* representation, marking the later representations as "Insertions".
    - Thus, if a user mutates the second iteration of a loop in their downstream editor, we catch the "Unmapped Insertion" and *cannot safely determine* if the change should apply to the singular `SourceTemplate` template rule. Thus, we degrade safely. Under these unalignable conditions, the algorithm bails into human-fallback and yields the `<<<< diff decision needed >>>>` block outlining the ambiguity.

## Usage

The workspace ships two crates: `burgertocow-lib` (the library, imported as `burgertocow`) and the `burgertocow` CLI.

### Library

The rendering surface mirrors `minijinja::Environment` so swapping `Tracker` in for a plain `Environment` is a local change:

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

For full control (filters, globals, loaders, autoescape, ...) reach through `tracker.env_mut()` — it's the raw `minijinja::Environment`.

### CLI

```bash
# Render with markers-stripped output:
burgertocow render --template tests/fixtures/simple.md --data tests/fixtures/simple_data.json --out out.md

# Reverse-diff the modifications:
burgertocow diff --template tests/fixtures/simple.md --data tests/fixtures/simple_data.json --modified mod.md
```

## Testing

The project has tests and text fixtures under `tests/fixtures/`. See `crates/burgertocow-lib/tests/integration.rs` for validations around variables modifications, simple text patches, and ambiguous loop catchers.

Run the same checks CI runs locally:

```bash
scripts/check         # fmt + clippy + tests
scripts/check-fmt     # rustfmt --check
scripts/check-lint    # clippy -D warnings
scripts/check-tests   # nextest (or cargo test)
```

To install the pre-commit hook:

```bash
ln -sf ../../scripts/pre-commit .git/hooks/pre-commit
```

## License

MIT — see [`LICENSE`](LICENSE).
