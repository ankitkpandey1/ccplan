# ccplan — Coding Conventions

The canonical coding standard for this project. It is grounded in the official Rust guidance and the
community patterns book — follow those by default and treat this file as the project-specific
tightening on top:

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) (naming, interoperability, predictability)
- [Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/) (formatting — enforced by `rustfmt`)
- [The Rust Programming Language](https://doc.rust-lang.org/book/) (idioms)
- [Rust Design Patterns](https://rust-unofficial.github.io/patterns/) (idioms, patterns, anti-patterns)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)

Both human contributors and AI agents must follow this. `CONTRIBUTING.md` points here.

---

## 1. Non-negotiables (the hard rules)

1. **No `unsafe`.** Crate root carries `#![forbid(unsafe_code)]`. Backends are chosen to avoid it
   (e.g. Windows scheduling shells out to `schtasks.exe` rather than `windows`-rs COM). If a future
   dependency forces an unsafe boundary it stays *inside that dependency*; document it in `notes.md`.
2. **Strong type safety — make illegal states unrepresentable.** (§3)
3. **SOTA + idiomatic, edition 2024.** No deprecated APIs; verify current usage during recon.
4. **`rustfmt` is the only formatting authority.** Never hand-format. CI runs `cargo fmt --check`.
5. **Clippy clean at `-D warnings`, with `clippy::pedantic` on.** Allow individual pedantic lints only
   with an inline `#[allow(clippy::…)]` **and a reason**.
6. **Comments explain WHY, never WHAT.** (§7)
7. **Tests drive code (TDD); coverage stays at 100%** of testable logic.
8. **Performance is a feature.** (§6)
9. **The CLI must not make the user think.** (§8)

---

## 2. Naming & formatting (per API Guidelines + Style Guide)

- Casing (RFC 430): `UpperCamelCase` types/traits/enum-variants; `snake_case` functions/methods/
  modules/locals; `SCREAMING_SNAKE_CASE` consts/statics. Acronyms are one word: `TomlStore`, not
  `TOMLStore`; `id`, not `ID`.
- Conversions follow the standard verbs: `as_` (cheap borrow→borrow), `to_` (expensive/owned),
  `into_` (consuming). Getters are `thing()`, not `get_thing()`.
- Iterator-producing methods are named `iter`, `iter_mut`, `into_iter`.
- One concept per name; prefer clear over short. No Hungarian notation, no `_t` suffixes.
- One responsibility per module; no junk-drawer `utils.rs`.
- Formatting is whatever `rustfmt` produces with the committed `rustfmt.toml`. `.editorconfig` mirrors it.

## 3. Type safety — make illegal states unrepresentable

- **Newtypes over primitives.** Wrap domain scalars: `BlockId(String)`, `Lead(Duration)`, `ClockTime`,
  `Rev([u8; 32])`. Never pass a bare `String`/`u64` where a domain type exists. This buys
  compiler-checked intent and a home for parsing/validation.
- **Enums over stringly/boolean typing.** `Status`, `Event { Notify, Start, End }`, `FireDecision`.
  No magic-string states; no bare `bool` parameters that need a comment to decode (use a 2-variant enum).
- **Parse, don't validate** ([Alexis King](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/)).
  Convert untrusted input (TOML, CLI args) into validated domain types **at the boundary**; downstream
  code receives only already-valid values and never re-checks. Model "exactly one of `end`/`duration`"
  as a single enum (`Span::Until(ClockTime) | Span::For(Lead)`) so "both" / "neither" cannot exist.
- **`TryFrom`/`FromStr` for every parsed scalar**, with parse logic + its tests co-located.
- **`#[non_exhaustive]`** on public enums/structs that may grow (`Error`, config), so additions aren't
  breaking changes.
- Prefer `Option`/`Result` and the type system over runtime asserts. Reserve `unreachable!()` for
  invariants the types already guarantee, and mark such arms `#[coverage(off)]` with a why-comment.
- Don't over-model: typestate/builders only where they genuinely prevent misuse.

## 4. Error handling

- **Library:** one crate `Error` enum via [`thiserror`](https://docs.rs/thiserror); functions return
  `Result<T, Error>`. Each variant carries the context needed to act on it, and maps to a documented
  process **exit code** (`error.rs`). `#[non_exhaustive]`.
- **Binary boundary only:** `anyhow` is acceptable in `main`/CLI glue for top-level reporting.
- Propagate with `?`; never silently discard a `Result` (clippy `must_use`). No `unwrap()`/`expect()`
  in library paths except type-guaranteed invariants; `main` and tests may `expect` with a message.
- Error messages are user-facing copy — see §8 (say what's wrong **and** the fix).

## 5. API & idioms (per API Guidelines + patterns book)

- Accept borrowed/generic inputs, return owned: take `impl AsRef<Path>` / `&str` / `impl IntoIterator`
  where it eases callers; don't demand `String`/`Vec` you'll only read.
- Derive the standard traits where they make sense: `Debug` (almost always), `Clone`, `PartialEq`/`Eq`,
  `Hash`, `Default`, plus `serde::{Serialize, Deserialize}` on data types. Public types are `Debug`.
- Implement `Display` for user-facing types; keep `Debug` developer-facing.
- Prefer iterators and combinators over manual index loops; prefer `match`/`if let`/`let else` over
  nested conditionals. Use `?`, `map_or`, `unwrap_or_default`, etc.
- **Dependency injection via traits** is the backbone pattern (testability): side effects are traits —
  `Clock`, `Scheduler`, `Notifier` — held in a `Context` and threaded through `run`. Prefer generics
  (`<S: Scheduler>`, monomorphized/zero-cost) or `&dyn` where object-safety/binary-size wins. Real
  impls are the only `#[coverage(off)]` code; tests inject fakes.
- **Pure core, imperative shell:** decision logic (`model`/`time`/`lifecycle`) is pure and total; IO
  (`store`/`platform`) lives at the edges. Determinism rule: never call `Zoned::now()`, spawn a
  process, or read `$HOME` in logic — always go through the injected capability.
- Avoid the **`Deref` polymorphism anti-pattern** and gratuitous trait objects; prefer plain functions
  and data. Keep modules acyclic.

## 6. Performance (a feature, not an afterthought)

Per the [Performance Book](https://nnethercote.github.io/perf-book/) — applied with judgment, never at
the cost of correctness or clarity in pure logic:

- **Fast startup.** The binary is a scheduler fire-target invoked many times a day. Do the minimum on
  the common path; don't read/parse files you don't need; no global lazy init that isn't required.
- **Don't clone to dodge the borrow checker.** Borrow (`&T`/`&str`/`&[T]`) over `clone()`/`to_owned()`;
  clone only when ownership is genuinely needed. `cargo clippy` (pedantic) flags many of these.
- **Allocate deliberately.** Reuse buffers; `&str` over `String`, `&[T]` over `Vec<T>` in signatures;
  prefer iterators (lazy) over building intermediate collections.
- **No needless blocking on the hot path** (the `fire` path especially). Notification/automation IO is
  unavoidable there, but everything around it should be lean.
- Measure before optimizing anything non-obvious; don't sacrifice readability of pure logic for
  micro-gains. But **never ship gratuitously wasteful code** — wasteful-by-default is a bug here.

## 7. Comments & documentation

- **Comments explain WHY, not WHAT.** A `//` comment states a design decision, trade-off, invariant,
  or non-obvious constraint — e.g. `// rev excludes status so the end-trigger stays valid (Inv-15)`.
  It must never paraphrase the code (`// increment counter`). If a comment restates the code, delete it
  and let the code speak; if the code is unclear, **rename** rather than annotate.
- **`///` doc-comments** on public items are encouraged — they document the *contract* (what it
  guarantees, errors, panics, units), which is API documentation for the reader, not what-narration.
  Cross-reference `DESIGN.md` invariants where relevant.
- Keep a module-level `//!` doc on each module stating its single responsibility.
- No commented-out code, no `TODO` without a `backlog.md` entry referencing it.

## 8. CLI UX — "Don't Make Me Think" (Steve Krug)

The interface must be self-evident; the common case needs almost no thought.

- **Obvious, consistent commands & flags.** Verbs read naturally (`add`, `show`, `apply`, `done`);
  flags are consistent across commands (`--json`, `--date`, `--dry-run` mean the same everywhere).
- **`--help` everywhere, and it's good.** Every command/subcommand has a clear help with examples
  (`clap` derive + `after_help`). `ccplan` with no args points the user somewhere useful.
- **Sensible defaults.** The common path works with minimal flags (today's date implied, notify lead
  defaulted). Destructive actions are the only ones that require explicit confirmation flags.
- **Error messages that teach.** State exactly what's wrong **and** how to fix it, with the offending
  value and a suggested next command — never a bare code or a stack trace. (e.g. `error: '/x/y' is not
  on the automation allowlist. Add it under [automation].allowed_executables in ~/.config/ccplan/config.toml`.)
- **Machine- and human-readable.** Human output is scannable; `--json` is stable and documented for
  agents. Reads that can match multiple items always return a JSON array.
- **`doctor` removes guesswork.** It diagnoses the scheduler/notifier/timezone setup and prints the
  exact fix, so the user never has to reverse-engineer why an alert didn't fire.
- Respect `NO_COLOR`; never require interactivity (no prompts) — agents and scripts must work unattended.

---

## 9. Quality gate (must pass before any commit)

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings      # pedantic on
cargo test --all-features --workspace
RUSTFLAGS="--cfg coverage_nightly" cargo +nightly llvm-cov --all-features --workspace --fail-under-lines 100
cargo deny check
```

The working rhythm is recon → implement → self-review → reflect, with the coverage gate kept honest
(no module-scope coverage exclusions that hide untested logic).
