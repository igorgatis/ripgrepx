# Proposals: candidate features

Status: **candidate ideas, not committed.** This page collects features that would make `rgx` more
useful for AI coding agents, with enough design to argue an implementation order. Nothing here is
built yet; each section ends with the open questions that still need answering.

## Why these, and the guiding constraint

The motivating evidence is real agent behavior. Across 49 Claude Code sessions on one project, the
model reached for raw `grep` **307 times** vs the built-in search tool **15**, and raw `find` **21**
vs **2** — despite an instruction rule *and* the installed rgx skill both saying "always use rgx."
Two things fall out of that data:

- **~70% of those greps were pipe filters** on command output (`curl … | grep -q 200`,
  `git branch | grep x`). Those are correctly *not* rgx's job and nothing here tries to absorb them.
- **The remaining ~30%** were repo content/file searches that rgx should win — and the single
  biggest lever is *relevance*, not raw speed: an agent typically reads only the first page of
  results, so page 1 has to be the answer.

Every candidate below preserves the core contract from [`design.md`](design.md): **matching is
ripgrep's; rgx only chooses which files ripgrep opens, and (in `--compact`/MCP) how results are
ordered and presented.** Ranking *reorders*, it never drops a match. Any feature that would change
the match set is scoped to an rgx-only mode and desugars to a plain `rg` query before matching.

---

## A. Weighted match (model-supplied relevance) — **implemented**

> Shipped as **`--sort=weight --weights=label:weight,...`** (not `--rankby` — see the note below),
> on the bare search, CLI `--compact`, and MCP `content_search`. The score→sort→page→cursor pipeline
> below is built; attribution uses capture groups resolved in the presentation layer (`src/rank.rs`),
> per-file **max** aggregation, unlabeled matches baseline 0 (sink last). See
> [`querying.md`](querying.md#ordering--sort--sortr), [`cli.md`](cli.md), and [`mcp.md`](mcp.md).
> Resolved open questions: `<label>` binds to the alternation branch / preceding run (group-postfix
> `(...)<w>` not yet supported); max aggregation; default weight 0; per-file ordering, lines kept in
> source order.
>
> **Surface change from this proposal.** Rather than a bespoke `--rankby`, ordering is unified under
> ripgrep's own `--sort`/`--sortr` (see section B): the filesystem keys (`path`, `modified`,
> `accessed`, `created`) match `rg` exactly, and `weight` is rgx's one extension, fed by `--weights`.
> This retires the proposal's `--rankby` entirely and makes weighted match work on the bare output too
> (`rgx --sort=weight … | head`), not just `--compact`.

**Motivation.** Agents already encode relevance by hand: 114 of the 307 greps used alternation
(`A\|B\|C`) to OR several candidate terms in one pass. The model knows which branch it *most* expects
to be the real hit — it just has no way to say so. Let it.

**Surface as shipped** (`--sort=weight` selects the order, `--weights` defines named weights; the
pattern references them with `<label>`):

```sh
rgx --sort=weight --weights=w1:0.7,w2:0.3 'Hello (world<w1>|earth<w2>)!'
```

- `--weights=w1:0.7,w2:0.3` declares labels and their weights; `--sort=weight` orders by them.
- `<w1>` / `<w2>` are postfix annotations on alternation branches (or atoms). rgx **strips** them and
  hands ripgrep the plain regex `Hello (world|earth)!`.
- A file's score is the weight of the branch that matched. Files hitting `world` (0.7) rank above
  files hitting `earth` (0.3), highest first.

**Contract.** Weighted match is an rgx-only key (active only with `--sort=weight`). The match set
equals the plain desugared regex's — exactly `rg`'s. Only ordering changes.

**Attribution** (which branch matched → which weight). Two routes, both available because confirm
calls the `grep`/`regex` crates directly:

1. **Capture groups** — wrap each labeled span in a capture group; the `regex` crate reports which
   group participated on a matching line. Single pass, no extra queries.
2. **Sub-queries** — run each labeled branch as its own accelerated search and tag results by label.
   This is just [batch search](#e-batch-search-mcp) with a weight attached, and reuses that engine.

**Why it's a good first slice.** It forces building the whole relevance *output* pipeline as one
vertical: compute a per-file score, sort the grouped `--compact` view by it, page in that order, and
encode the order into the keyset cursor so pages stay stable. Once that machinery exists, every other
ranking signal (below) is an incremental add.

**Honest caveat.** Unlike the automatic signals in section B, weighted match is a *model-supplied*
signal, so it **cannot be backtested** against historical transcripts (the model never wrote weighted
patterns). It's a bet that letting the model express intent improves page 1 — validated going
forward, not from existing data.

**Open questions** (most now resolved — see the **implemented** note above; remaining):
- Group-postfix binding `(...)<w>` is not yet supported (v1 binds `<label>` to the alternation branch
  / preceding run); whether to extend it.
- A configurable baseline for unlabeled matches (today fixed at 0, sink last).

---

## B. Relevance signal framework (generalizes A) — **partially implemented**

> **Surface decided: extend `--sort`, don't invent `--rankby`.** The ordering flag is ripgrep's
> `--sort`/`--sortr`. Its filesystem keys (`path`, `modified`, `accessed`, `created`) are **shipped**
> and match `rg` exactly — so `last-modified` below is just `rg --sort=modified` and was never an rgx
> invention to reinvent. `weight` is shipped (section A). The remaining signals below are rgx-only
> additions to the `--sort` vocabulary (no rg equivalent), still to build.

**Motivation.** Weight is one relevance signal; there are cheaper automatic ones the agent doesn't
have to annotate. The real question — *which signal matters most* — is answered by architecture
(the unified `--sort` ordering layer) plus the experiment in section C, not by guessing.

**Surface.** `--sort=KEY` / `--sortr=KEY` (single key today). A future **lexicographic sort chain**
(primary, then tie-break) would extend the same flag; a final `path:line` tiebreak already gives a
deterministic total order (so keyset paging is stable).

```sh
rgx --sortr=modified PAT                       # shipped (rg's key)
rgx --sort=weight --weights=a:0.9,b:0.1 PAT    # shipped (rgx extension)
rgx --sort=git-changed PAT                     # proposed (rgx extension)
rgx --sort=folder-distance:server/lib PAT      # proposed (rgx extension)
```

**Signal vocabulary.**

| Signal (`--sort=…`) | Needs | Status | Note |
| --- | --- | --- | --- |
| `path` / `modified` / `accessed` / `created` | `stat` | **shipped** | ripgrep's keys, exact parity |
| `weight` | weighted pattern (A) + `--weights` | **shipped** | per-branch match weight |
| `git-changed` | `git diff --name-only` | proposed | dirty/branch-changed first; pairs with section D |
| `git-recency` | `git log` recency | proposed | "touched recently in history" |
| `folder-distance[:path]` | a reference path | proposed | tree-distance to cwd or `:path` |
| `in-folder:path` | a path | proposed | partition: files under `path` first |
| `match-count` | confirm result | proposed | denser files first |
| `path-kind` | path heuristic | proposed | src > test > vendored/generated |

**Defaults — a real design point.** Over MCP there is no per-call cwd, so reference-based signals
(`folder-distance`) can't be the default. A future default MCP ranking should be **reference-free**
(`git-changed,modified` — "what you're working on, then recently touched"), with an optional
`reference_path` param (e.g. the file the agent is editing) to unlock `folder-distance`. The CLI keeps
today's stable order unless `--sort` is passed, so scripts don't shift.

**The unit-test concern.** Test files are sometimes noise, sometimes the target — so test handling is
never hardcoded. `path-kind` is opt-in and only *deprioritizes* tests (they sink, but stay present
and reachable on later pages). Looking *for* tests? Omit `path-kind`, or add `in-folder:test`.

**Contract.** Pure reordering; the match set is never changed (filesystem keys match `rg --sort`
exactly, the rgx-only keys reorder the `--compact`/MCP and bare views). Never drops.

**Open questions.** Per-file vs per-match granularity for per-match signals; git signal cost (shell
`git` vs libgit2 vs reading `.git`); buffering bound when materializing the ordered file list for a
huge result set; whether `path-kind` classification is heuristic or configurable.

---

## C. Relevance eval harness (find the differentiators with numbers)

**Motivation.** "Relevance will make a huge difference — but which signal?" is answerable from data,
not intuition. The label is free: after a search, the file the agent next **Read/Edited** is the
relevance target.

**Sketch.**
1. From session transcripts, extract `(query, files matched, file the agent touched next)` triples.
2. Replay each query's candidate set; score it under each signal and combination from section B.
3. Report **precision@1 / MRR** per signal: does it put the eventually-touched file on page 1, ideally
   at rank 1? The winners become the default ranking; the harness doubles as a regression guard.

**Scope note.** This validates the *automatic* signals (mtime, folder-distance, git, path-kind,
match-count). It cannot score weighted match (A), which needs model-supplied annotations absent from
historical data. So C is the evidence base for the *defaults*; A is a separate forward-looking bet.

**Open questions.** Which transcripts form the corpus; how to attribute "the file the agent touched"
when several reads follow one search; whether to weight recent sessions more.

---

## D. Git-scoped search

**Motivation.** Review/verify workflows recur throughout the transcripts (searching only touched
files, `grep -c "^+" /tmp/review.diff`). Two distinct uses:

- **Scope filter** — `rgx --changed PAT` / `rgx --since <ref> PAT`. Semantically identical to
  `rg PAT $(git diff --name-only <ref>)`: a path restriction, so matching stays `rg`'s
  (contract-safe), and even *fallback* (non-trigram) queries get fast because the path set is tiny.
- **Ranking signal** — `git-changed` / `git-recency` feed section B (reorder, don't drop).

**Change-set semantics (define precisely).**
- `--changed`: working tree (staged + unstaged + untracked).
- `--since <ref>`: committed diff `ref..HEAD` (optionally `+working`).
- Default ref for "what's on my branch": merge-base with the default branch.

**Surface.** rgx-recognized leading-token modifiers (`--changed`, `--since <ref>`), consistent with
`--compact`; neither collides with an `rg` flag. MCP: `changed_only` / `since` on content search.

**Open questions.** Working-tree vs branch-diff vs both as default; submodules; renamed files; how a
non-git directory behaves (error vs no-op).

---

## E. Batch search (MCP)

**Motivation.** The hand-built `A\|B\|C` alternations and `;`-joined multi-search one-liners are the
model minimizing round-trips by hand. Make it first-class: one call, N labeled result sets. Also the
clean attribution route for weighted match (A).

**Surface (MCP content search).** Add `queries: Array<{ pattern, label? }>`, mutually exclusive with
`pattern`; shared modifiers (`case_insensitive`, `word`, `fixed_strings`, `page_size`, `files_only`,
`count`) apply to all. `label` defaults to the pattern.

**Response.** Concatenated per-query `--compact` blocks, each with its own header and true total:

```
[batch: 3 queries]
== "useAuth" -- [matches 1-12 of 12 in 5 files] ==
...
== "SignIn" -- [matches 1-50 of 88 in 20 files]  (page-walk: re-run this query alone) ==
...
```

**Paging (v1).** Batch is for breadth/orientation: each query returns page 1 + total; if one
overflows, the agent re-issues *that* query with the existing single-pattern cursor. No multi-cursor
in v1. `files_only` / `count` batch variants ("which of these 8 symbols exist, and where") are the
sweet spot and are cheap.

**Performance.** One warm index, N queries; candidate resolution is bitmap ops; confirm fans out
concurrently over small candidate sets. **Contract.** Each query's match set is exactly `rg`'s; no
cross-query dedup (a shared line under two queries is correct to show twice).

**Open questions.** Per-query flag overrides vs shared-only; a unified batch cursor later; cap on N.

---

## F. Read-hints and freshness

**Motivation.** The transcripts show a constant two-round-trip dance: `grep -n SYMBOL | head` to
locate, then `sed -n '18,40p'` to read around it. Collapse it.

- **Read-hints (v1).** Every `--compact`/MCP file group carries its matched **line span**
  (`lines: 18-40`); MCP returns structured `ranges`. The agent's follow-up read is scoped in one shot.
- **Peek (v2).** Opt-in `--peek=block` returns the **enclosing scope** per match via language-agnostic
  brace/indent heuristics — smarter than a fixed `-C 3`.
- **Freshness flag (finish what [`mcp.md`](mcp.md) already promises).** At confirm, compare each
  result file's current `(size, mtime)` to the index entry; if it drifted since the last sync, mark
  the file header (`~ path  (changed since index)`). Output stays disk-accurate; the marker just warns
  "re-read before editing by line number."

**Contract.** Additive metadata on the `--compact`/MCP view; bare `rgx` stays byte-for-byte `rg`.

**Open questions.** Per-match ranges vs per-file span; how far `peek=block` heuristics go without a
real parser; keep the freshness marker out of bare output (compact/MCP only).

---

## G. Adoption note (docs + install, not a search feature)

A skill is *pull* (loaded when the model judges it relevant); a line in the always-loaded
`CLAUDE.md` / `AGENTS.md` / `GEMINI.md` is *push* (always in context). For a blanket "never grep,
always rgx" rule, push wins — which is exactly what the 307-vs-15 data shows. Note that
`--agent install` **already** writes an always-on block for Codex/VS Code/Cursor/Gemini; **Claude is
the only target that gets a pull-only skill.** Two cheap fixes:

- README + `rgx --agent --help`: note that a one-line rule in the host's always-loaded instructions
  file is what makes adoption stick; the skill is the reference it pulls in.
- `rgx --agent install claude`: optionally offer a *marked, removable* one-liner in `CLAUDE.md`
  (opt-in, since CLAUDE.md is user-authored), mirroring the Codex/Copilot blocks.

---

## Suggested order

Two viable orderings; they differ on whether to lead with the feature you want or the experiment that
picks the defaults.

**Preferred (build the machinery first — leads with weighted match):**

1. **A. Weighted match.** Smallest vertical slice that builds the relevance output pipeline (score ->
   sort -> page -> cursor). High learning value; proves the architecture.
2. **B. Signal framework.** Generalize A: turn "weight" into one of several pluggable `--sort`
   keys; the filesystem keys (`modified`, …) ship with it, then add `folder-distance`, `git-changed`.
3. **C. Eval harness.** Now there are signals to compare — measure precision@1/MRR on real
   transcripts and lock in the defaults.
4. **D. Git-scoped search.** Independent and contract-safe; `git-changed` from B pairs with it.
5. **E. Batch search (MCP).** Independent breadth win; retroactively offers A an alt attribution path.
6. **F. Read-hints + freshness.** Polish that makes page 1 land; small, can interleave anywhere.
7. **G. Adoption note.** Doc/install change, do whenever.

**Data-first alternative (lets numbers pick the defaults before betting on weights):** C (on the
automatic signals) -> B -> A -> D -> E -> F. Stronger evidence story, but defers the feature you're
most interested in and starts with measurement infrastructure rather than a shippable feature.

**Recommendation: go with the preferred order, starting with A.** Not merely because it's the most
wanted feature, but because A is a genuine vertical slice rather than throwaway scaffolding: building
it forces the entire relevance *output* pipeline — per-file score, sort the `--compact` view, page in
that order, and encode the order into the keyset cursor. B and every later signal reuse exactly that
machinery, so "weighted first" doubles as "ranking infrastructure first."

The one adjustment worth making: **pull C in to slot between A and B.** A's score/sort/page/cursor
pipeline gives the harness something to measure; C then ranks the *automatic* signals (mtime,
folder-distance, git-changed, path-kind) on real transcripts, so B ships with evidence-backed
defaults instead of guessed ones. A remains a forward-looking bet (model-supplied weights can't be
backtested); C de-risks everything around it. Net order: **A → C → B → D → E → F**, with G whenever.
