# Cortex — growth & distribution kit

Staged, ready-to-fire materials for growing the project. Nothing here auto-posts; you publish each consciously.

## Contents

| Path | What | Action |
|------|------|--------|
| `../docs/social-preview.html` | 1280×640 OG card source | open + screenshot |
| `../docs/social-preview.png` | Rendered OG image (2560×1280 @2x) | upload at Settings → Social preview |
| `discussions/*.md` | 3 seed GitHub Discussions | `bash post-discussions.sh` |
| `post-discussions.sh` | One-command publisher for the 3 discussions | run when ready |
| `launch/show-hn.md` | Show HN title + first comment | post to news.ycombinator.com |
| `launch/reddit-localllama.md` | r/LocalLLaMA post | post |
| `launch/reddit-rust.md` | r/rust post | post |
| `listings/submission-kit.md` | MCP registries + awesome-list entries | submit PRs/forms |

## Already done (live)

- ✅ GitHub `homepage` → live WASM demo
- ✅ Repo description → unified `156µs` ingest figure
- ✅ µs benchmark numbers reconciled across README_CN / x-article (canonical: 156µs ingest, 568µs search)
- ✅ Star CTA in `cortex-http` startup banner + persistent button on the demo page
- ✅ `server.json` MCP tool count 27 → 29
- ✅ Community files: CONTRIBUTING, CODE_OF_CONDUCT, issue/PR templates

## Suggested order

1. **Upload the OG image** (one manual step, no API) — makes every shared link look good *before* you drive traffic.
2. **Post the 3 Discussions** — so the Discussions tab isn't empty when visitors arrive.
3. **Submit to MCP registries** (`listings/submission-kit.md`) — passive, compounding discovery.
4. **Then** the Show HN / Reddit launch — only after 1–3, so the landing experience is polished.
