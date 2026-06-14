# Renders docs/demo.gif from REAL cortex-mcp-server commands (no faked output).
#   vhs docs/demo.tape
Output docs/demo.gif

Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 660
Set Padding 22
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms

# --- hidden setup: PATH + a clean `cx` wrapper (hides the long path and the one-time
#     model-load notice on stderr); fresh demo DB so output is curated, not stale. ---
Hide
Type "export PATH=$HOME/.local/bin:$PATH; rm -f /tmp/demo.db*" Enter
Type 'cx() { cortex-mcp-server /tmp/demo.db "$@" 2>/dev/null; }' Enter
Type "clear" Enter
Sleep 400ms
Show

Sleep 800ms
Type "# Day 1 — tell your AI something worth remembering:" Enter
Sleep 700ms
Type "cx ingest 'I deploy on Fly.io and always run tests before pushing'" Enter
Sleep 3.2s

Type "# A brand-new session later (new process, empty context). Ask:" Enter
Sleep 700ms
Type "cx search 'how do I ship this project?'" Enter
Sleep 4s

Type "# Remembered — 100% local. No cloud, no API key, no telemetry." Enter
Sleep 3s
