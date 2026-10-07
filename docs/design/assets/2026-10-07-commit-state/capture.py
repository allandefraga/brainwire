"""Capture real offline TUI views of a disposable 100,000-Customer graph.

Run with: uv run --with pyte --with pillow python docs/design/assets/2026-10-07-commit-state/capture.py
Requires the current target/debug/brainwire. Only synthetic data; no server.
Every import batch contains at most 5,000 rows. PTYs and temporary stores are
closed on success or failure. PNGs render the actual terminal escape stream.
"""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time

import pyte
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent
BINARY = ROOT / "target/debug/brainwire"
FONT = ImageFont.truetype(
    os.environ.get("BRAINWIRE_CAPTURE_FONT", "/System/Library/Fonts/Menlo.ttc"), 18
)
CELL_W, CELL_H = 11, 24
PALETTE = {
    "black": "#070b09", "red": "#ef5350", "green": "#57ff48",
    "brown": "#d1b657", "blue": "#6da8e8", "magenta": "#b294db",
    "cyan": "#67d9d2", "white": "#d9ece4", "brightblack": "#67766d",
    "brightred": "#ff7777", "brightgreen": "#57ff48", "brightyellow": "#ffe489",
    "brightblue": "#8ac8ff", "brightmagenta": "#d4bbff",
    "brightcyan": "#9eefff", "brightwhite": "#ffffff",
}


def color(value, background=False):
    if value == "default":
        return "#070b09" if background else "#d9ece4"
    if value in PALETTE:
        return PALETTE[value]
    if len(value) == 6:
        return "#" + value
    return "#070b09" if background else "#d9ece4"


def capture(view, lang, env, sequence):
    cols, rows = 160, 48
    name = f"offline-{view}-{lang}-{cols}x{rows}"
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def terminal_session():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    process = subprocess.Popen(
        [str(BINARY), "shell", "--overview", "--graph", "customers-100k",
         "--view", view, "--lang", lang],
        stdin=slave, stdout=slave, stderr=slave, cwd=ROOT, env=env,
        close_fds=True, preexec_fn=terminal_session,
    )
    os.close(slave)
    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)

    def drain(seconds):
        stop = time.monotonic() + seconds
        while time.monotonic() < stop:
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                stream.feed(data)
                if b"\x1b[6n" in data:
                    os.write(master, b"\x1b[1;1R")

    try:
        # Exact offline counts scan the graph before entering the terminal.
        for _ in range(30):
            drain(0.3)
            if "BRAINWIRE" in "\n".join(screen.display):
                break
            assert process.poll() is None, "TUI exited before first frame"
        drain(0.4)
        text = "\n".join(screen.display)
        assert "BRAINWIRE" in text and "customers-100k" in text, text
        assert "offline" in text.lower(), text
        if view not in ("ontology", "disk"):
            assert "100000" in text, text
        if view == "query":
            assert str(sequence) in text, text
            assert "db.commitState()" in text, text
            assert "replication" in text if lang == "en" else "replicação" in text, text
            assert "no query execution" in text if lang == "en" else "não executa consultas" in text, text
        (OUT / f"{name}.txt").write_text(text + "\n")
        image = Image.new("RGB", (cols * CELL_W, rows * CELL_H), "#070b09")
        draw = ImageDraw.Draw(image)
        for y in range(rows):
            for x in range(cols):
                cell = screen.buffer[y][x]
                fg, bg = color(cell.fg), color(cell.bg, True)
                if cell.reverse:
                    fg, bg = bg, fg
                draw.rectangle((x * CELL_W, y * CELL_H,
                                (x + 1) * CELL_W - 1, (y + 1) * CELL_H - 1), fill=bg)
                if cell.data and cell.data != " ":
                    draw.text((x * CELL_W, y * CELL_H), cell.data, font=FONT, fill=fg)
        image.save(OUT / f"{name}.png")
        print(name, "captured", flush=True)
    finally:
        if process.poll() is None:
            os.write(master, b"q")
            drain(0.4)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        os.close(master)
    assert process.returncode == 0, process.returncode
    return name


def main():
    started = time.time()
    with tempfile.TemporaryDirectory(prefix="brainwire-commit-state-capture-") as temporary:
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(TERM="xterm-256color", BRAINWIRE_SERVER_DATA_DIR=temporary + "/data",
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB")
        env.pop("NO_COLOR", None)

        def run(*args):
            return subprocess.run([str(BINARY), *args], cwd=ROOT, env=env,
                                  check=True, capture_output=True, text=True, timeout=60)

        run("db", "create", "customers-100k", "--ontology",
            str(ROOT / "crates/core/tests/fixtures/graph_mapping.json"))
        directory = Path(temporary) / "data/customers-100k"
        assert directory.exists(), "named database directory was not created"
        parameters = Path(temporary) / "batch.json"
        query = ("UNWIND $rows AS row MERGE (c:Customer {entity_id: row.entity_id}) "
                 "SET c.email = row.email, c.segment = row.segment")
        for start in range(0, 100_000, 5_000):
            parameters.write_text(json.dumps({"rows": [
                {"entity_id": f"synthetic-customer-{i:06}",
                 "email": f"customer-{i:06}@example.test", "segment": "synthetic"}
                for i in range(start, start + 5_000)
            ]}))
            run("cypher", "--database", str(directory), "--query", query,
                "--params-file", str(parameters))
            print(f"Committed synthetic rows: {start + 5_000}", flush=True)
        stats = json.loads(run("db", "stats", "customers-100k", "--json").stdout)
        state = json.loads(run("db", "commit-state", "customers-100k", "--json").stdout)
        assert stats["nodes"] == 100_000 and stats["edges"] == 0, stats
        assert state["tracked"] and state["scope"] == "store_latest", state
        sequence = int(state["sequence"])
        names = [capture(view, "en", env, sequence)
                 for view in ("graph", "query", "memory", "disk", "ontology")]
        names.append(capture("query", "pt-BR", env, sequence))
        final_state = json.loads(run("db", "commit-state", "customers-100k", "--json").stdout)
        assert final_state == state, (state, final_state)
        receipt = {
            "scope": "actual disposable offline graph; synthetic customers",
            "nodes": stats["nodes"], "edges": stats["edges"], "stats": stats,
            "commit_state": state, "commit_counter_is_node_count": False,
            "batch_rows": 5000, "batches": 20, "terminal": {"columns": 160, "rows": 48},
            "binary_sha256": hashlib.sha256(BINARY.read_bytes()).hexdigest(),
            "binary": "target/debug/brainwire", "views": names,
            "configured_cache": "64MiB", "configured_memtables": "16MiB",
            "elapsed_seconds": round(time.time() - started, 3),
            "temporary_store_removed_on_exit": True,
            "capture_source": "PTY escape stream rendered with pyte and Pillow",
            "counter_explanation": "Per-store commits include ID allocator and ontology writes; not a node count or global replication position.",
        }
    (OUT / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print("PTYs closed; temporary storage removed; receipt saved.", flush=True)


if __name__ == "__main__":
    main()
