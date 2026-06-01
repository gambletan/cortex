# Contributing to Cortex

Thanks for your interest in Cortex — a private, local-first memory engine for personal AI agents. Contributions of all sizes are welcome: bug reports, docs, benchmarks, NLP rules, and features.

## Ground rules

- **Privacy is the product.** No telemetry, no phone-home, no third-party data egress. PRs that add network calls outside explicit, user-enabled cloud sync will be declined.
- **Local-first stays local.** Anything that requires a hosted service to function belongs behind an opt-in flag, never in the default path.
- **Keep it lean.** Cortex ships as a 3.8 MB binary with zero runtime dependencies. New crate dependencies need a clear justification.

## Project layout

| Crate | What it is |
|-------|------------|
| `cortex-core` | Memory engine — tiers, retrieval, beliefs, inference, sync |
| `cortex-http` | REST API server (`axum`) + embedded dashboard |
| `cortex-mcp-server` | Model Context Protocol server (29 tools) for LLM clients |
| `cortex-wasm` | Browser build (124 KB) powering the live demo |
| `cortex-python` | Python SDK (`pip install cortex-ai-memory`) |
| `bench/` | LoCoMo harness and benchmark data |

## Dev workflow

```bash
# Build everything
cargo build --workspace

# Run the full test suite (489+ tests)
cargo test --workspace

# Microbenchmarks (ingest/search/beliefs)
cargo bench -p cortex-core

# Run the HTTP server locally
cargo run -p cortex-http -- --port 3315

# Lint + format before pushing
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

## Pull requests

1. Fork and branch from `main` (`git checkout -b feature/my-thing`).
2. Add or update tests — behavior changes without test coverage won't be merged.
3. Run `cargo fmt`, `cargo clippy`, and `cargo test --workspace` locally; all must pass.
4. Keep commits focused and write a clear PR description (what + why).
5. If you touch performance-sensitive paths, include before/after `cargo bench` numbers.

## Reporting bugs

Open an issue using the **Bug report** template. Include OS, Cortex version (`cortex-http --version` or the crate version), reproduction steps, and what you expected.

## Benchmarks & claims

Performance numbers in the README come from `cargo bench` on an M-series Mac and **include proactive inference on every ingest**. If you submit new numbers, state the machine and the methodology so results stay comparable.

## License

By contributing, you agree your contributions are licensed under the [MIT License](LICENSE).
