"""Capture the actual offline TUI in a controlling PTY; requires pyte and Pillow.

Run from any directory with uv run --with pyte --with pillow python capture.py.
The optional --existing-100k flag also opens the local synthetic staging-100k graph.
Default captures use temporary databases, removed after the run. Fonts default
to macOS Menlo; BRAINWIRE_CAPTURE_FONT can point to another monospace font.
"""

import argparse
import fcntl
import os
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path
import pyte
from PIL import Image, ImageDraw, ImageFont

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--existing-100k", action="store_true")
args = parser.parse_args()
root = Path(__file__).resolve().parents[4]
out = root / "docs/design/assets/2026-10-04-ontology"
out.mkdir(parents=True, exist_ok=True)
font = ImageFont.truetype(
    os.environ.get("BRAINWIRE_CAPTURE_FONT", "/System/Library/Fonts/Menlo.ttc"), 18
)
cell_w, cell_h = 11, 24
palette = {
    "black": "#070b09",
    "red": "#ef5350",
    "green": "#57ff48",
    "brown": "#d1b657",
    "blue": "#6da8e8",
    "magenta": "#b294db",
    "cyan": "#67d9d2",
    "white": "#d9ece4",
    "default": "#d9ece4",
    "brightblack": "#67766d",
    "brightred": "#ff7777",
    "brightgreen": "#57ff48",
    "brightyellow": "#ffe489",
    "brightblue": "#8ac8ff",
    "brightmagenta": "#d4bbff",
    "brightcyan": "#9eefff",
    "brightwhite": "#ffffff",
}


def color(value, background=False):
    if value == "default":
        return "#070b09" if background else "#d9ece4"
    if value in palette:
        return palette[value]
    if len(value) == 6:
        return "#" + value
    return "#070b09" if background else "#d9ece4"


def capture(name, cols, rows, lang, graph, keys, env, resize=None):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def terminal_session():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    p = subprocess.Popen(
        [
            str(root / "target/debug/brainwire"),
            "shell",
            "--overview",
            "--view",
            "ontology",
            "--graph",
            graph,
            "--lang",
            lang,
        ],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        cwd=root,
        env=env,
        close_fds=True,
        preexec_fn=terminal_session,
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
        drain(0.7)
        assert p.poll() is None, f"TUI exited early: {p.returncode}"
        for key in keys:
            os.write(master, key)
            drain(0.2)
        if resize:
            cols, rows = resize
            screen.resize(rows, cols)
            fcntl.ioctl(
                master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0)
            )
            drain(0.3)
        text = "\n".join(screen.display)
        assert "BRAINWIRE" in text, text
        assert "ONTOLOG" in text, text
        if "inspector" in name and "scrolled" not in name:
            assert "PROPERTIES" in text, text
        if "scrolled" in name:
            assert "OUTGOING" in text and "WORKS_AT" in text, text
        if "staging-100k" in name:
            assert "Page" in text and "LINK" in text, text
        if "company" in name:
            assert "> Company" in text, text
        (out / (name + ".txt")).write_text(text + "\n")
        image = Image.new("RGB", (cols * cell_w, rows * cell_h), "#070b09")
        draw = ImageDraw.Draw(image)
        for y in range(rows):
            for x in range(cols):
                c = screen.buffer[y][x]
                fg, bg = color(c.fg), color(c.bg, True)
                if c.reverse:
                    fg, bg = bg, fg
                draw.rectangle(
                    (
                        x * cell_w,
                        y * cell_h,
                        (x + 1) * cell_w - 1,
                        (y + 1) * cell_h - 1,
                    ),
                    fill=bg,
                )
                if c.data and c.data != " ":
                    draw.text((x * cell_w, y * cell_h), c.data, font=font, fill=fg)
        image.save(out / (name + ".png"))
        print(name, "captured", flush=True)
    finally:
        if p.poll() is None:
            os.write(master, b"q")
            drain(0.5)
            os.close(master)
            master = None
            try:
                p.wait(timeout=3)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()
        if master is not None:
            os.close(master)
    assert p.returncode == 0, f"TUI failed on exit: {p.returncode}"


with tempfile.TemporaryDirectory(prefix="brainwire-ontology-capture-") as tmp:
    env = dict(
        os.environ,
        TERM="xterm-256color",
        BRAINWIRE_SERVER_DATA_DIR=tmp + "/data",
        BRAINWIRE_MEMORY_CACHE="64MiB",
        BRAINWIRE_MEMORY_MEMTABLES="16MiB",
    )
    env.pop("NO_COLOR", None)
    for name, schema in [
        ("ontology", root / "crates/core/tests/fixtures/graph_mapping.json"),
        ("knowledge", root / "examples/knowledge/schema.json"),
    ]:
        subprocess.run(
            [
                str(root / "target/debug/brainwire"),
                "db",
                "create",
                name,
                "--ontology",
                str(schema),
            ],
            env=env,
            cwd=root,
            check=True,
            capture_output=True,
        )
    capture("ontology-en-160x40", 160, 40, "en", "ontology", [], env)
    capture("ontology-pt-BR-160x40", 160, 40, "pt-BR", "ontology", [], env)
    capture("ontology-company-160x40", 160, 40, "en", "ontology", [b"\x1b[B"], env)
    capture("ontology-en-80x24", 80, 24, "en", "ontology", [], env)
    capture("ontology-inspector-80x24", 80, 24, "en", "ontology", [b"\r"], env)
    capture(
        "ontology-inspector-scrolled-80x24",
        80,
        24,
        "en",
        "ontology",
        [b"\r", b"\x1b[6~", b"\x1b[6~"],
        env,
    )
    capture(
        "ontology-resized-inspector-80x24",
        160,
        40,
        "en",
        "ontology",
        [b"\r"],
        env,
        resize=(80, 24),
    )
    capture("ontology-isolated-160x40", 160, 40, "en", "knowledge", [], env)
    capture("ontology-empty-80x24", 80, 24, "en", "default", [], env)

# Existing synthetic benchmark graph; offline, no graph writes.
if args.existing_100k:
    env = dict(os.environ, TERM="xterm-256color")
    env.pop("NO_COLOR", None)
    capture(
        "ontology-staging-100k-160x40",
        160,
        40,
        "en",
        "staging-100k",
        [],
        env,
    )
