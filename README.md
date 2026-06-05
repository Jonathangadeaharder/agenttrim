# agenttrim

Rust CLI for pruning unused AI agent artifacts. Analyzes project files, identifies stale skills/instructions, and removes them safely.

## Stack

Rust (edition 2021), MIT licensed.

## Build

```bash
cargo build
cargo test
cargo clippy -- -D warnings
```

## Structure

```
src/
├── main.rs          # CLI entrypoint
├── lib.rs           # Core logic
├── analyze/         # Artifact analysis
├── prune/           # Pruning logic
├── shared/          # Shared types
└── time_provider.rs # Time-based heuristics
```
