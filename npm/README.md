# ripgrepx (`rgx`)

**Instant ripgrep for codebases you search over and over.**

`ripgrepx` installs the `rgx` command: a trigram-indexed front end to ripgrep that jumps straight to
the candidate files, while ripgrep still does the matching — so results are byte-for-byte `rg`'s, just
faster. It also has a token-frugal `--compact` paged view and an MCP server, built for AI agents.

```sh
npm install -g ripgrepx        # installs the `rgx` command
rgx 'fn .*Handler' src/        # accelerated ripgrep search
rgx --compact 'TODO'           # grouped + paged, token-savings view
rgx --skill                    # install the agent skill
```

On install, this package downloads the matching prebuilt binary (macOS, Linux, Windows; x64 + arm64)
from the [GitHub release](https://github.com/igorgatis/ripgrepx/releases). The binary is
self-contained — ripgrep is linked in, so you do not need `rg` installed.

Full docs: https://github.com/igorgatis/ripgrepx
