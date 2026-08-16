# Contributing

Keep changes scoped to the host, wire protocol, or content-blind relay. The
private Apple clients consume this repository through a pinned release and are
not part of public pull requests.

Before opening a pull request, run:

```bash
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
(cd relay && npm ci && npm test && npm run typecheck)
```

Protocol changes require updated versioned schema documentation and fixtures.
Do not log prompts, model output, source files, tool arguments, private keys,
pairing tokens, or relay channel secrets.
