# ripgrep ignore & scope — the parity spec

`rgx` narrows which files get scanned; ripgrep does the matching. For that to be sound, the candidate
set `rgx` confirms must be **exactly** the file set ripgrep would walk for the same invocation — no
more (or `rgx` emits matches `rg` wouldn't), no less (or `rgx` misses matches). This document is the
authoritative description of how ripgrep chooses that file set, so we can make `rgx` byte-for-byte
identical, just faster.

Source of truth: the [`ignore`](https://docs.rs/ignore) crate, version **0.4.26** — the exact engine
`rgx` links (`Cargo.lock`). All `file:line` citations are into
`~/.cargo/registry/.../ignore-0.4.26/src/`. ripgrep version probed: **15.1.0**. When in doubt, the
code wins; reproduce with the experiments in [§4](#4-reproducible-experiments).

---

## 1. Search scope — which roots ripgrep walks

- `rg PATTERN` with no path searches the **current directory** (`./`). `rg PATTERN p1 p2 …` walks each
  given path independently. Scope is the positional path(s), never a flag.
- Output paths are printed **relative to the root you gave** (`rg PATTERN src/` prints `src/foo.rs`;
  run from inside `src/`, `rg PATTERN` prints `foo.rs`).
- **The roots you name are exempt from all ignore/hidden/filesize filtering.** In the walker,
  `skip_entry` returns `false` immediately for any entry at depth 0 (`walk.rs:1057-1060`):
  ```rust
  fn skip_entry(&self, ent: &DirEntry) -> Result<bool, Error> {
      if ent.depth() == 0 { return Ok(false); }   // a root you handed in — never skipped
      if should_skip_entry(&self.ig, ent) { return Ok(true); }
      ...
  ```
  Ignore/hidden rules (`should_skip_entry`, `walk.rs:1070`, `1769`, defined at `walk.rs:1939`) apply
  **only to entries discovered by descending** (depth ≥ 1). So `rg PATTERN build/` searches `build/`
  even when `build/` is git-ignored, and `cd node_modules/pkg && rg PATTERN` searches it. **This is
  the one behavior that a parent-rooted index cannot reproduce by filtering** (see [§5](#5-implications-for-rgx)).

---

## 2. The ignore decision — precedence ladder

For a descended entry, `Ignore::matched` (`dir.rs:401-445`) consults sources highest-precedence first;
the first source that yields a decision wins. Within the ignore-file group the order is the `.or()`
chain at `dir.rs:580-586` (`Match::or` keeps the first non-`None`, `lib.rs:479-481`):

| # | Source | Git-gated? | Citation |
|---|---|---|---|
| 1 | **Overrides** — `-g/--glob`, `--iglob` (short-circuit, whitelist or ignore) | no | `dir.rs:412-425` |
| 2 | **Custom ignore files** — `--ignore-file-name`; ripgrep registers `.rgignore` here | no | `dir.rs:285-297` |
| 3 | **`.ignore`** | no | `dir.rs:298-309` |
| 4 | **`.gitignore`** | **yes** | `dir.rs:310-321` |
| 5 | **`.git/info/exclude`** | **yes** | `dir.rs:323-342` |
| 6 | **Global gitignore** — `core.excludesFile` | **yes** | `dir.rs:571-578` |
| 7 | **Explicit `--ignore-file`** | no | `dir.rs:565-570` |
| 8 | **Type filters** — `-t/--type`, `--type-not` | no | `dir.rs:435-443` |
| 9 | **Hidden fallback** — skip dotfiles, only if nothing above matched | no | `dir.rs:386-395` |

One-line ladder: `-g` ▸ custom/`.rgignore` ▸ `.ignore` ▸ `.gitignore` ▸ `.git/info/exclude` ▸ global ▸
`--ignore-file` ▸ type ▸ hidden.

### 2.1 Git activation — `.gitignore` is inert without a repo

Sources 4–6 are **dead unless a `.git` (or `.jj`) is found in the directory chain.** The per-directory
`.gitignore` matcher is still *built*, but consulted only when `any_git` holds (`dir.rs:461-462`):
```rust
let any_git = !self.inner.opts.require_git
    || self.parents().any(|ig| ig.inner.has_git);
```
`require_git` defaults to `true` (`dir.rs:685`); `has_git` is set when a directory contains `.git`
(dir **or** file — linked worktrees, resolved via `resolve_git_commondir`, `dir.rs:941-985`) or `.jj`
(`dir.rs:273-282`). So **in a plain directory with a `.gitignore` but no `.git`, the `.gitignore` does
nothing** (test `gitignore_no_git`, `dir.rs:1078-1088`; contrast `gitignore`, `dir.rs:1052-1063`).
`rg --no-require-git` disables the gate (`dir.rs:1090-1103`). `.ignore`/custom/hidden are **not**
git-gated — they work anywhere.

### 2.2 Parent walk-up and the git barrier

`add_parents` (`dir.rs:182-248`) loads ignore files from **every** ancestor up to the filesystem root
(`dir.rs:208-214`) — there is no early stop *during loading*. The git boundary is enforced at *match*
time by a `saw_git` cutoff (`dir.rs:463`, `480`, `487`, `494`, `561`):

- `.ignore` / custom-ignore from **all** ancestors apply (no cutoff).
- `.gitignore` / `.git/info/exclude` apply only **up to and including the first `.git`-bearing
  ancestor** — a `.gitignore` *above* a nested repo's `.git` does not reach inside it (test
  `stops_at_git_dir`, `dir.rs:1244-1265`).

Net effect inside a repo: the `.gitignore` stack applied to `repo/src/foo.rs` is identical whether you
invoke `rg` from `repo/` or from `repo/src/` — both read every `.gitignore` from the file's directory
up to the repo root. (Confirmed empirically, [§4](#4-reproducible-experiments) exp. 1.)

### 2.3 Negation, directory-only, pruning

- `!pattern` whitelists (`gitignore.rs:471-487`). **Within one file, last match wins** — globs are
  scanned in reverse and the highest-line match decides (`gitignore.rs:248-271`, esp. `:260`).
- `pattern/` matches directories only (`is_only_dir`, enforced at `gitignore.rs:262`).
- **The "a file under an excluded directory cannot be re-included" gitignore rule is enforced by the
  *walker*, not the matcher**: when a directory matches `Ignore`, the walker never descends into it,
  so a deeper `!sub/file` is unreachable. The single-file matcher answers per-path only.
- Across sources, the higher-precedence source decides first (a `.ignore` `!foo` whitelist overrides a
  `.gitignore` `foo`; tests `ignore_over_gitignore`, `exclude_lowest`).

### 2.4 Hidden and symlinks

- **Hidden** (`hidden(true)` default, `dir.rs:678-679`): an entry whose basename starts with `.` is
  skipped — but only as a *fallback* after ignore matching (`dir.rs:386-395`), so an explicit `!`
  whitelist un-hides it. `is_hidden` is basename-first-byte `==` `.` on Unix (`pathutil.rs:11-19`);
  Windows also honors the HIDDEN attribute.
- **Symlinks** are **not followed** by default (`follow_links: false`, `walk.rs:554`). A symlink given
  explicitly as a *file* root is read (`walk.rs:578`). Symlink-following is orthogonal to ignore.

---

## 3. The complete file-set rule

For `rg [flags] PATTERN ROOT…`, the searched set is:

1. For each `ROOT`: yield `ROOT` itself unconditionally (depth-0 exemption, §1), then
2. recursively descend, and at each descended entry apply the ladder (§2): overrides → custom →
   `.ignore` → (`.gitignore` → exclude → global, **iff** a `.git`/`.jj` is in the chain) →
   `--ignore-file` → type filters → hidden; a matched directory is pruned (not descended).
3. Default modifiers: dotfiles skipped, symlinks not followed, `--max-filesize` off. Flags
   (`--hidden`, `--no-ignore`, `-u/-uu`, `-g`, `-t`, `--follow`, `--max-filesize`) change the set.

---

## 4. Reproducible experiments

Run against ripgrep 15.1.0 (`/opt/homebrew/bin/rg`). These pin the load-bearing facts:

1. **Parent `.gitignore` applies from a subdir.** Repo with root `.gitignore` = `*.log`; from
   `repo/src`, `rg -l NEEDLE` returns `a.rs` only, not `debug.log`. → git-stack read up to the root
   regardless of invocation dir (§2.2).
2. **Explicit ignored path is searched.** `.gitignore` = `build/`; `rg -l NEEDLE build/` returns
   `build/out.txt`; `cd node_modules/pkg && rg -l NEEDLE` returns `dep.js`. → depth-0 exemption (§1).
3. **`.gitignore` without `.git` is inert.** A non-repo dir with `.gitignore` = `*.log`: `rg -l NEEDLE`
   returns **both** `keep.txt` and `skip.log`. → git activation gate (§2.1).

---

## 5. Implications for rgx

The contract: `rgx`'s candidate set for a query must **equal** ripgrep's walked set for the same
invocation. Confirm runs the matcher on an explicit file list and does **not** re-apply ignores, so
both under- and over-inclusion are correctness bugs.

What this spec dictates if we share one index across a repo's subdirectories:

1. **Index root = the git repository root** (the dir containing `.git`/`.jj`), else cwd. This is not
   just convenient — per §2.1 it is what *activates* `.gitignore`, and per §2.2 it is the barrier the
   parent walk-up stops at. Rooting the index there reproduces ripgrep's exact ignore stack for every
   subdirectory query. One index per repo, ignore-faithful by construction.
2. **Scope = the path arg (else cwd).** Filter index candidates to that subtree and print relative to
   it. Within the subtree the ignore decision is identical to `rg` run from the subtree (§2.2), so the
   filtered set matches — **except** the case below.
3. **The depth-0 exemption is the one divergence** (§1). When the scope is *at or under* an
   ignored/hidden directory (`node_modules/`, `build/`, `.git/`, a dotdir), a git-root index pruned it
   to nothing, but `rg` invoked from there searches it. Reuse must **detect this and fall back** to a
   direct ripgrep run (or a scope-rooted index) for that query. Detection: record the directories the
   index walk pruned; scope is unsafe iff it is at/under a pruned dir.
4. **Non-default ignore flags bypass the index.** The index is built for one ignore configuration;
   `--no-ignore`, `--hidden`, `-u/-uu`, `--follow`, custom `-g`/`--ignore-file` change the set, so
   those queries fall back to a full ripgrep scan (the existing "unaccelerable" path).
5. **Validate with a differential fuzzer** before shipping: random trees with random
   `.gitignore`/`.ignore`/hidden/nested/negated layouts, random subdir scopes, asserting
   `rgx(pattern, scope) == rg(pattern) run from scope` byte-for-byte. Seed the known-divergent cases
   (scope under an ignored dir; nested repos/submodules/worktrees, since `.git`-as-file and the
   `saw_git` barrier interact; a `.gitignore` at the scope dir; no-git directories). MISSED must be 0
   and there must be no extra matches.
