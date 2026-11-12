# fieldtrip
GraphQL introspection and execution over MCP. Agents find real operations, plan and cost them, and run them with your approval.

## Development

The toolchain is pinned in `rust-toolchain.toml` (the MSRV is always the latest stable), so `rustup` picks it up
automatically. Lints are configured in `Cargo.toml` and are warnings locally; CI fails on any warning.

```sh
cargo fmt --all                                                   # 120-column formatting
cargo clippy --workspace --all-targets --all-features             # pedantic + nursery + curated restriction lints
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
cargo test --workspace --all-features
```

Every public item needs a doc comment; functions returning `Result` need an `# Errors` section and functions that
can panic need a `# Panics` section. To silence a lint locally, use `#[expect(lint, reason = "...")]` rather than
`#[allow]`.
