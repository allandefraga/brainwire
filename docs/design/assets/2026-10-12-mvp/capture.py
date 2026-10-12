"""Capture live Memory and Indexes, including same-session refresh recovery.

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
import hashlib
import secrets
import subprocess
import tempfile
import termios
import time
from pathlib import Path
import pyte
from PIL import Image, ImageDraw, ImageFont

root = Path(__file__).resolve().parents[4]
out = root / "docs/design/assets/2026-10-12-mvp"
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


def capture(view, cols, rows, lang, stages, env):
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
            view,
            "--graph",
            "tickets",
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
        for name, keys, before_keys in stages:
            if before_keys:
                before_keys()
                drain(1.2)
            for key in keys:
                os.write(master, key)
                drain(0.5)
            text = "\n".join(screen.display)
            assert "BRAINWIRE" in text and ("SERVER SNAPSHOT" in text or "SNAPSHOT DO SERVIDOR" in text), text
            assert "CALL db." + view + "()" in text, text
            if "stale" in name:
                assert "STALE" in text and "refresh failed" in text, text
                assert "67108864 B" in text, text
            if "recovered" in name:
                assert "STALE" not in text and "67108864 B" in text, text
            if view == "indexes":
                assert "READY" in text or "PRONTO" in text, text
                if "scrolled" in name:
                    assert "Type12" in text and "/ 20" in text, text
            (out / (name + ".txt")).write_text(text + "\n")
            image = Image.new("RGB", (cols * cell_w, rows * cell_h), "#070b09")
            draw = ImageDraw.Draw(image)
            for y in range(rows):
                for x in range(cols):
                    c = screen.buffer[y][x]
                    fg, bg = color(c.fg), color(c.bg, True)
                    if c.reverse:
                        fg, bg = bg, fg
                    draw.rectangle((x * cell_w,y * cell_h,(x + 1) * cell_w - 1,(y + 1) * cell_h - 1),fill=bg)
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


with tempfile.TemporaryDirectory(prefix="brainwire-live-mvp-capture-") as tmp:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
    env.update(TERM="xterm-256color", BRAINWIRE_SERVER_DATA_DIR=tmp + "/data",BRAINWIRE_SERVER_DEFAULT_DATABASE="tickets", BRAINWIRE_MEMORY_CACHE="64MiB",BRAINWIRE_MEMORY_MEMTABLES="16MiB",BRAINWIRE_MEMORY_QUERY="64MiB")
    env.pop("NO_COLOR", None)
    binary = str(root / "target/debug/brainwire")
    token = secrets.token_hex(32)
    policy = Path(tmp) / "policy.json"
    client = Path(tmp) / "admin.json"
    policy.write_text(json.dumps({"principals":[{"id":"capture","token_sha256":hashlib.sha256(token.encode()).hexdigest(),"graphs":{"tickets":"read"},"instance_admin":True}]}))
    client.write_text(json.dumps({"principal":"capture","credentials":token}))
    for path in [policy,client]:
        path.chmod(0o600)
    env.update(BRAINWIRE_SERVER_AUTH_FILE=str(policy), BRAINWIRE_BOLT_AUTH_FILE=str(client))
    def run(*args):
        return subprocess.run([binary,*args],env=env,cwd=root,check=True,capture_output=True,text=True)
    mapping = {"version":"1","nodes":[{"label":f"Type{i:02}","properties":["name"],"indexed_fields":["name"]} for i in range(20)],"edges":[]}
    schema=Path(tmp)/"schema.json";schema.write_text(json.dumps(mapping))
    run("db","create","tickets","--ontology",str(schema))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1",0));address="%s:%s"%sock.getsockname()
    server=None
    def stop():
        global server
        if server is not None:
            server.kill();server.wait(timeout=5);server=None
    def start():
        global server
        server=subprocess.Popen([binary,"serve"],env=dict(env,BRAINWIRE_SERVER_LISTEN=address),cwd=root,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        for _ in range(100):
            if subprocess.run([binary,"probe","--address",address,"--timeout-ms","100"],env=env,capture_output=True).returncode==0:
                return
            if server.poll() is not None:
                raise RuntimeError("capture server exited before readiness")
            time.sleep(0.03)
        raise RuntimeError("capture server did not become ready")
    try:
        start()
        capture("memory",120,30,"en",[("live-memory-en-120x30",[],None),("live-memory-stale-en-120x30",[b"r"],stop),("live-memory-recovered-en-120x30",[b"r"],start)],env)
        capture("memory",80,24,"pt-BR",[("live-memory-pt-BR-80x24",[],None)],env)
        capture("indexes",120,30,"en",[("live-indexes-en-120x30",[],None),("live-indexes-scrolled-en-120x30",[b"\x1b[B"]*12,None)],env)
        capture("indexes",80,24,"pt-BR",[("live-indexes-pt-BR-80x24",[],None)],env)
    finally:
        stop()
    stats=json.loads(run("db","stats","tickets","--json").stdout)
    assert stats["nodes"]==0 and stats["edges"]==0, stats
    print("Synthetic schema only; no record writes; server stopped; temporary credentials and storage removed.")
