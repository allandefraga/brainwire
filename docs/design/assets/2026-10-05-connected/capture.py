"""Capture connected TUI, refresh failure and recovery against an isolated server.

Requires pyte/Pillow and the built debug brainwire CLI. MacOS Menlo is the
default font; BRAINWIRE_CAPTURE_FONT can override it. No real graph data used.
"""

import fcntl
import os
import pty
import select
import struct
import socket
import json
import subprocess
import tempfile
import termios
import time
from pathlib import Path
import pyte
from PIL import Image, ImageDraw, ImageFont

root = Path(__file__).resolve().parents[4]
out = root / "docs/design/assets/2026-10-05-connected"
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


def capture(name, cols, rows, lang, graph, keys, env, before_keys=None):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def terminal_session():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    p = subprocess.Popen(
        [
            str(root / "target/debug/brainwire"),
            "shell",
            "--connect",
            address,
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
        if before_keys:
            before_keys()
        for key in keys:
            os.write(master, key)
            drain(0.5)
        text = "\n".join(screen.display)
        assert "BRAINWIRE" in text, text
        assert "ONTOLOG" in text, text
        assert "Customer" in text, text
        if "stale" in name:
            assert "STALE" in text and "refresh failed" in text, text
        if "refreshed" in name:
            assert "schema v2" in text and "Ticket" in text, text
            assert "STALE" not in text, text
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


with tempfile.TemporaryDirectory(prefix="brainwire-connected-capture-") as tmp:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
    env.update(
        TERM="xterm-256color",
        BRAINWIRE_SERVER_DATA_DIR=tmp + "/data",
        BRAINWIRE_MEMORY_CACHE="64MiB",
        BRAINWIRE_MEMORY_MEMTABLES="16MiB",
    )
    env.pop("NO_COLOR", None)
    binary = str(root / "target/debug/brainwire")

    def run(*args):
        return subprocess.run(
            [binary, *args],
            env=env,
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        )

    run(
        "db",
        "create",
        "tickets",
        "--ontology",
        str(root / "crates/core/tests/fixtures/graph_mapping.json"),
    )
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = "%s:%s" % sock.getsockname()
    server = None

    def stop():
        global server
        if server is not None:
            server.kill()
            server.wait(timeout=5)
            server = None

    def start():
        global server
        server = subprocess.Popen(
            [binary, "serve"],
            env=dict(env, BRAINWIRE_SERVER_LISTEN=address),
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for _ in range(100):
            result = subprocess.run(
                [binary, "probe", "--address", address, "--timeout-ms", "100"],
                env=env,
                capture_output=True,
            )
            if result.returncode == 0:
                return
            time.sleep(0.03)
        raise RuntimeError("server did not start")

    def upgrade():
        stop()
        mapping = json.loads(
            (root / "crates/core/tests/fixtures/graph_mapping.json").read_text()
        )
        mapping["version"] = "2"
        mapping["nodes"].append({"label": "Ticket", "properties": ["title", "status"]})
        mapping["edges"].append(
            {"type": "OPENED", "from": {"label": "Customer"}, "to": {"label": "Ticket"}}
        )
        path = Path(tmp) / "updated.json"
        path.write_text(json.dumps(mapping))
        run("db", "apply-ontology", "tickets", str(path))
        start()

    try:
        start()
        capture("connected-en-160x40", 160, 40, "en", "tickets", [], env)
        capture("connected-pt-BR-80x24", 80, 24, "pt-BR", "tickets", [], env)
        capture(
            "connected-stale-160x40",
            160,
            40,
            "en",
            "tickets",
            [b"r"],
            env,
            before_keys=stop,
        )
        start()
        capture(
            "connected-stale-40x24",
            40,
            24,
            "en",
            "tickets",
            [b"r"],
            env,
            before_keys=stop,
        )
        start()
        capture(
            "connected-refreshed-160x40",
            160,
            40,
            "en",
            "tickets",
            [b"r"],
            env,
            before_keys=upgrade,
        )
    finally:
        stop()
    stats = json.loads(run("db", "stats", "tickets", "--json").stdout)
    assert stats["nodes"] == 0 and stats["edges"] == 0, stats
    print("No instance writes; server stopped; temporary storage removed on exit.")
