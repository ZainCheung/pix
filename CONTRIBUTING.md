# Contributing

Keep changes scoped to the host, wire protocol, content-blind relay, or the
public macOS menu-bar client. The private iOS client consumes this repository
through a pinned release and is not part of public pull requests.

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
(cd relay && npm ci && npm test && npm run typecheck)
```

Product releases use the workspace version and a matching `vX.Y.Z` tag. See
[docs/RELEASE.md](docs/RELEASE.md) for the release and Relay deployment
workflow.

Protocol changes require updated versioned schema documentation and fixtures.
Do not log prompts, model output, source files, tool arguments, private keys,
pairing tokens, or relay channel secrets.
