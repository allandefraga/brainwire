# Connected MVP terminal captures

The `live-*.png` images are actual interactive terminal cells from the built
Brainwire CLI, captured through a controlling PTY, decoded with pyte and rendered
with Pillow and macOS Menlo. They are not concept images. Matching `live-*.txt`
files preserve the captured cells. The other snapshot text files in this folder
are produced by the separate acceptance harness.

The capture uses temporary synthetic storage with 20 node types and one declared
scalar equality index per type. It starts a protected native Bolt server and a
random service token with read access to `tickets` plus `instance_admin` for the
Memory pane. No node or edge records are written. Credentials and storage are
removed on exit; child server and TUI processes are stopped in cleanup handlers.

- `live-memory-en-120x30`: shared server query-pool reservation counters.
- `live-memory-stale-en-120x30`: server killed, then `r` pressed; old data retained.
- `live-memory-recovered-en-120x30`: server restarted, then `r` pressed in that same
  TUI process; error cleared and snapshot age reset.
- `live-memory-pt-BR-80x24`: Portuguese compact Memory pane.
- `live-indexes-en-120x30`: first page of configured indexes.
- `live-indexes-scrolled-en-120x30`: twelve Down keys; selected `Type12`, updated
  visible range and advancing snapshot age.
- `live-indexes-pt-BR-80x24`: Portuguese compact index list and state explanations.

All seven PNGs were inspected after generation. Counters and selected rows come
from the actual server response; ports, sample ages and reservation peaks can
change between runs. Byte counters show KiB/MiB/GiB with two decimals and exact
bytes in parentheses; counters below 1024 retain bytes only. Server restart resets its lifetime peak/rejection counters.

Reproduce from the repository root:

```sh
cargo build -p brainwire-server --locked --offline
uv run --no-project --with pyte --with pillow python docs/design/assets/2026-10-12-mvp/capture.py
```

The script defaults to `/System/Library/Fonts/Menlo.ttc`; set
`BRAINWIRE_CAPTURE_FONT` to a local monospace TrueType font on another OS.
`pyte` and Pillow are capture-only dependencies, not application dependencies.

Manual refresh currently fetches synchronously, so keyboard input can wait until
the configured request deadline (at most 10 seconds). These captures verify
normal refresh failure and recovery; they do not measure responsiveness during a
slow server request or prove a process-memory/RSS limit. `READY` indexes support
the implemented scalar equality path; the pane does not claim complex-value,
range, vector or full-text index support.
