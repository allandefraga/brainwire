"""Record connected Brainwire views and a decision recall on disposable data."""

import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time

import pyte
from PIL import Image, ImageDraw, ImageFont

OUT = Path(__file__).resolve().parent
ROOT = OUT.parents[1]
sys.path.insert(0, str(ROOT))

from brainwire import connect
from examples.python.create_service_tokens import create

COLS, ROWS = 114, 29
CELL_W, CELL_H = 10, 19
W, H = 1280, 800
PAPER, INK, BLUE = "#f0f3fc", "#18213a", "#3159f3"
TERM_BG, TERM_FG = "#0d1120", "#e6eaff"
MONO = "/System/Library/Fonts/Menlo.ttc"
SANS = "/System/Library/Fonts/SFNS.ttf"
ANSI_COLORS = {
    "black": "#141c2d", "red": "#ff767b", "green": "#65eab1",
    "brown": "#f6cf77", "blue": "#7998ff", "magenta": "#bd9dff",
    "cyan": "#89e3ef", "white": "#d9e5ff", "brightblack": "#8b9bc1",
    "brightred": "#ff9d9f", "brightgreen": "#9cf1c3", "brightyellow": "#ffe4a0",
    "brightblue": "#adc0ff", "brightmagenta": "#dbbcff", "brightcyan": "#b5f3f9",
    "brightwhite": "#f1f4ff",
}


def color(value, fallback):
    if len(value) == 6 and all(c in "0123456789abcdefABCDEF" for c in value):
        return f"#{value}"
    return ANSI_COLORS.get(value, fallback)


def frame(title, version, subtitle, position, mono, sans):
    image = Image.new("RGB", (W, H), PAPER)
    canvas = ImageDraw.Draw(image)
    canvas.text((68, 30), "BRAINWIRE  /  LIVE DEMO", font=ImageFont.truetype(mono, 17), fill=BLUE)
    canvas.text((68, 66), title, font=ImageFont.truetype(sans, 53), fill=INK)
    canvas.rounded_rectangle((44, 151, W - 44, 743), radius=22, fill=TERM_BG)
    canvas.text((68, 164), "BOLT SESSION    ·    SYNTHETIC DATA", font=ImageFont.truetype(mono, 15), fill="#a9b8e5")
    canvas.text((W - 68, 164), version, anchor="ra", font=ImageFont.truetype(mono, 15), fill="#a9b8e5")
    canvas.text((68, 760), subtitle, font=ImageFont.truetype(sans, 20), fill=INK)
    canvas.text((W - 68, 765), position, anchor="ra", font=ImageFont.truetype(mono, 16), fill=BLUE)
    return image, canvas


def terminal_frame(screen, title, version, subtitle, position, mono, sans):
    image, canvas = frame(title, version, subtitle, position, mono, sans)
    font = ImageFont.truetype(mono, 16)
    for y in range(ROWS):
        for x in range(COLS):
            cell = screen.buffer[y][x]
            fg, bg = color(cell.fg, TERM_FG), color(cell.bg, TERM_BG)
            if cell.reverse:
                fg, bg = bg, fg
            px, py = 68 + x * CELL_W, 191 + y * CELL_H
            if bg != TERM_BG:
                canvas.rectangle((px, py, px + CELL_W - 1, py + CELL_H - 1), fill=bg)
            if not cell.data or cell.data == " ":
                continue
            if len(cell.data) == 1 and 0x2800 <= ord(cell.data) <= 0x28FF:
                # Draw the actual Braille dots where Menlo lacks the glyph.
                mask = ord(cell.data) - 0x2800
                for bit, (dx, dy) in enumerate(((3, 3), (3, 7), (3, 11), (7, 3), (7, 7), (7, 11), (3, 15), (7, 15))):
                    if mask & (1 << bit):
                        canvas.ellipse((px + dx - 1, py + dy - 1, px + dx + 1, py + dy + 1), fill=fg)
            else:
                canvas.text((px, py), cell.data, font=font, fill=fg)
    return image


def capture_workspace(binary, address, env, version, mono, sans):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def controlling_terminal():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    command = [binary, "shell", "--connect", address, "--graph", "company_demo",
               "--node-label", "Project", "--entity-id", "project-release",
               "--edge-type", "HAS_DECISION", "--target-label", "Decision"]
    try:
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env,
                                   cwd=ROOT, preexec_fn=controlling_terminal)
    except BaseException:
        os.close(master)
        raise
    finally:
        os.close(slave)
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)

    def drain(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if not select.select([master], [], [], 0.05)[0]:
                continue
            try:
                data = os.read(master, 65536)
            except OSError:
                return
            if not data:
                return
            stream.feed(data)
            if b"\x1b[6n" in data:
                os.write(master, b"\x1b[1;1R")

    try:
        drain(0.9)
        if process.poll() is not None:
            raise RuntimeError(f"TUI startup failed: {screen.display}")
        stages = [
            (b"1", "tui-ontology", "See the shape of a project", "Connected Ontology pane", ("ONTOLOGY", "Decision", "Project"), "03 / 05"),
            (b"4", "tui-graph", "Follow the decisions", "Connected Graph pane · outgoing HAS_DECISION relationships", ("HAS_DECISION", "decision-timeout", "project-release"), "04 / 05"),
            (b"2", "tui-memory", "Inspect the memory budget", "Connected Memory pane · query reservation counters", ("MEMORY", "REJECTIONS"), "05 / 05"),
        ]
        images = []
        for key, name, title, caption, expected, position in stages:
            os.write(master, key)
            drain(0.7)
            screen_text = "\n".join(screen.display)
            if process.poll() is not None or any(word not in screen_text for word in expected):
                raise RuntimeError(f"Unexpected {name} screen: {screen_text}")
            png = OUT / f"{name}.png"
            terminal_frame(screen, title, version, caption, position, mono, sans).save(png)
            images.append(png)
        os.write(master, b"q")
        drain(0.3)
        if process.wait(timeout=3) != 0:
            raise RuntimeError("TUI failed on exit")
        return images
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)


def capture_agent_memory(address, env, version, mono, sans):
    fixture_arguments = ["remember", "--project", "project-release", "--title", "Retry failed requests",
                         "--body", "Use exponential backoff.", "--source-title", "Release meeting",
                         "--source-url", "https://example.com/meeting/release"]
    prefix = [sys.executable, "-m", "examples.python.agent_memory"]
    address_arguments = ["--uri", "bolt://" + address]
    write = subprocess.check_output(prefix + address_arguments + fixture_arguments, cwd=ROOT, env=env, text=True)
    recorded = json.loads(write)
    recall_arguments = ["recall", "--project", "project-release", "--limit", "1"]
    result = subprocess.check_output(prefix + address_arguments + recall_arguments, cwd=ROOT, env=env, text=True)
    recalled = json.loads(result)
    if (len(recalled["records"]) != 1 or recalled["records"][0]["title"] != fixture_arguments[4]
            or recalled["records"][0]["source_url"] != fixture_arguments[-1]
            or any(recalled["records"][0][key] != recorded[key] for key in ("decision_id", "source_id"))):
        raise RuntimeError("Agent recall returned unexpected decision or source")
    font = ImageFont.truetype(mono, 20)
    figures = [
        ("agent-remember", "Save a decision with its source", "01 / 05", write,
         ["$ python -m examples.python.agent_memory remember \\",
          "    --project project-release \\",
          "    --title 'Retry failed requests' \\",
          "    --body 'Use exponential backoff.' \\",
          "    --source-title 'Release meeting' \\",
          "    --source-url 'https://example.com/meeting/release'"]),
        ("agent-recall", "Recall a decision. See its source.", "02 / 05", result,
         ["$ python -m examples.python.agent_memory " + " ".join(recall_arguments)]),
    ]
    images = []
    for name, title, position, output, command_lines in figures:
        image, canvas = frame(title, version, "Actual Python CLI output · local endpoint omitted · long IDs shortened",
                              position, mono, sans)
        for number, line in enumerate(command_lines):
            canvas.text((68, 206 + 27 * number), line, font=font, fill="#a3b9ff")
        for number, line in enumerate(output.splitlines(), 1):
            if '"decision_id":' in line or '"source_id":' in line:
                line = line[:49] + '…"' + ("," if line.endswith(",") else "")
            canvas.text((68, 219 + 27 * (len(command_lines) + number - 1)), line, font=font, fill=TERM_FG)
        png = OUT / f"{name}.png"
        image.save(png)
        images.append(png)
    return images


@contextlib.contextmanager
def disposable_server(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-launch-demo-") as tmp:
        folder = Path(tmp)
        auth = create(folder / "auth", "company_demo", include_observer=True)
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.pop("NO_COLOR", None)
        env.update(TERM="xterm-256color", BRAINWIRE_SERVER_DATA_DIR=str(folder / "data"),
                   BRAINWIRE_SERVER_DEFAULT_DATABASE="company_demo", BRAINWIRE_MEMORY_QUERY="64MiB",
                   BRAINWIRE_SERVER_AUTH_FILE=str(auth / "policy.json"),
                   BRAINWIRE_BOLT_AUTH_FILE=str(auth / "observer.json"))
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            address = "%s:%s" % probe.getsockname()
        subprocess.run([binary, "db", "create", "company_demo", "--ontology",
                        str(ROOT / "examples/company_brain/ontology.json")],
                       check=True, capture_output=True, env=env, cwd=ROOT)
        with (folder / "server.log").open("wb") as log:
            server = subprocess.Popen([binary, "serve"], env=dict(env, BRAINWIRE_SERVER_LISTEN=address),
                                      cwd=ROOT, stdout=log, stderr=log)
            try:
                for _ in range(150):
                    if server.poll() is not None:
                        raise RuntimeError("Disposable server exited before readiness")
                    status = subprocess.run([binary, "probe", "--address", address, "--timeout-ms", "100"],
                                            env=env, capture_output=True, cwd=ROOT)
                    if status.returncode == 0:
                        break
                    time.sleep(0.03)
                else:
                    raise RuntimeError("Disposable server readiness timeout")
                writer = json.loads((auth / "writer.json").read_text())
                with connect("bolt://" + address, "company_demo",
                             auth=(writer["principal"], writer["credentials"]), max_transaction_retry_time=0) as graph:
                    graph.upsert_node("Team", "team-platform", {"name": "Platform"})
                    graph.upsert_node("Project", "project-release", {"name": "Release"})
                    graph.upsert_edge(("Team", "team-platform"), "OWNS", ("Project", "project-release"))
                    for name, title in (("decision-timeout", "Bound request time"),
                                        ("decision-alerts", "Alert on retries"),
                                        ("decision-queue", "Buffer bursts")):
                        graph.upsert_node("Decision", name, {"title": title})
                        graph.upsert_edge(("Project", "project-release"), "HAS_DECISION", ("Decision", name))
                yield address, env, dict(env, BRAINWIRE_BOLT_AUTH_FILE=str(auth / "writer.json"))
            finally:
                if server.poll() is None:
                    server.terminate()
                    try:
                        server.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait(timeout=5)


def encode(images):
    with tempfile.TemporaryDirectory(prefix="brainwire-launch-video-") as tmp:
        sequence = Path(tmp) / "sequence.txt"
        sequence.write_text("".join(f"file '{path.name}'\nduration {duration}\n"
                                    for path, duration in zip(images, (4, 5, 3, 5, 3), strict=True))
                            + f"file '{images[-1].name}'\n")
        for path in images:
            (Path(tmp) / path.name).symlink_to(path)
        video = OUT / "brainwire-live.mp4"
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "concat",
                        "-i", str(sequence), "-t", "20", "-vf", "fps=24,format=yuv420p", "-c:v", "libx264",
                        "-crf", "19", "-movflags", "+faststart", str(video)], check=True)
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(video), "-lavfi",
                        "fps=4,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
                        "-loop", "0", str(OUT / "brainwire-live.gif")], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--mono", default=MONO)
    parser.add_argument("--sans", default=SANS)
    args = parser.parse_args()
    binary = str(args.binary.resolve(strict=True))
    version = subprocess.check_output([binary, "--version"], text=True).strip()
    with disposable_server(binary) as (address, observer_env, writer_env):
        images = capture_agent_memory(address, writer_env, version, args.mono, args.sans)
        images.extend(capture_workspace(binary, address, observer_env, version, args.mono, args.sans))
    encode(images)
    print(f"Captured {version}: {', '.join(path.name for path in images)}, MP4 and GIF")


if __name__ == "__main__":
    main()
