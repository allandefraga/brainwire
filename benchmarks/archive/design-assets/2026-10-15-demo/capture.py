"""Capture real connected workspace cells against an isolated synthetic graph.

Capture-only dependencies: neo4j, pyte, Pillow. No source data or credentials
are preserved. All fixture writes happen before the read-only TUI is launched.
"""
import argparse
import fcntl
import hashlib
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
from neo4j import GraphDatabase
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
from examples.python.create_service_tokens import create

PALETTE = {"black":"#070b09", "red":"#ef5350", "green":"#57ff48", "brown":"#d1b657", "blue":"#6da8e8", "magenta":"#b294db", "cyan":"#67d9d2", "white":"#d9ece4", "brightblack":"#67766d", "brightred":"#ff7777", "brightgreen":"#57ff48", "brightyellow":"#ffe489", "brightblue":"#8ac8ff", "brightmagenta":"#d4bbff", "brightcyan":"#9eefff", "brightwhite":"#ffffff"}


def color(value, background=False):
    if value == "default":
        return "#070b09" if background else "#d9ece4"
    if value in PALETTE:
        return PALETTE[value]
    if len(value) == 6:
        return "#" + value
    return "#070b09" if background else "#d9ece4"


def capture(binary, address, cols, rows, lang, stages, env, font):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def terminal_session():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    process = subprocess.Popen([str(binary), "shell", "--connect", address,
        "--graph", "tickets", "--lang", lang,
        "--node-label", "Ticket", "--entity-id", "ticket-high-degree",
        "--edge-type", "HAS_COMMENT", "--target-label", "TicketComment"],
        stdin=slave, stdout=slave, stderr=slave, cwd=ROOT, env=env,
        close_fds=True, preexec_fn=terminal_session)
    os.close(slave)
    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    names = []

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
        drain(1.0)
        assert process.poll() is None, f"TUI exited before capture: {process.returncode}; {screen.display}"
        for name, keys, before_keys, expected in stages:
            if before_keys:
                before_keys()
                drain(0.2)
            for key in keys:
                os.write(master, key)
                drain(0.7)
            text = "\n".join(screen.display)
            assert process.poll() is None, f"TUI exited during navigation: {text}"
            for value in expected:
                assert value in text, f"{name}: missing {value}: {text}"
            assert "BRAINWIRE" in text and "Tab" in text, text
            if "recovered" in name:
                assert "STALE" not in text, text
            (OUT / (name + ".txt")).write_text(text + "\n")
            image = Image.new("RGB", (cols * 11, rows * 24), "#070b09")
            draw = ImageDraw.Draw(image)
            for y in range(rows):
                for x in range(cols):
                    cell = screen.buffer[y][x]
                    fg, bg = color(cell.fg), color(cell.bg, True)
                    if cell.reverse:
                        fg, bg = bg, fg
                    draw.rectangle((x*11,y*24,(x+1)*11-1,(y+1)*24-1), fill=bg)
                    if cell.data and cell.data != " ":
                        if len(cell.data) == 1 and 0x2800 <= ord(cell.data) <= 0x28ff:
                            # Menlo lacks Braille; render the actual cell's Unicode dots.
                            mask = ord(cell.data) - 0x2800
                            for bit, (dx, dy) in enumerate([(3,3),(3,9),(3,15),(8,3),(8,9),(8,15),(3,21),(8,21)]):
                                if mask & (1 << bit):
                                    draw.ellipse((x*11+dx-1,y*24+dy-1,x*11+dx+1,y*24+dy+1),fill=fg)
                        else:
                            draw.text((x*11,y*24), cell.data, font=font, fill=fg)
            image.save(OUT / (name + ".png"))
            names.append(name)
            print(name, "captured", flush=True)
    finally:
        if process.poll() is None:
            try:
                os.write(master, b"q")
                drain(0.5)
                process.wait(timeout=3)
            except (OSError, subprocess.TimeoutExpired):
                process.kill()
                process.wait(timeout=5)
        os.close(master)
    assert process.returncode == 0, f"TUI failed on exit: {process.returncode}"
    return names


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--nodes", type=int, default=1000)
    parser.add_argument("--binary", type=Path, default=ROOT/"target/release/brainwire")
    args = parser.parse_args()
    if not 22 <= args.nodes <= 100000:
        parser.error("--nodes must be 22..100000 to qualify the 20-row bound")
    binary = args.binary.resolve(strict=True)
    font = ImageFont.truetype(os.environ.get("BRAINWIRE_CAPTURE_FONT", "/System/Library/Fonts/Menlo.ttc"), 18)
    schema = ROOT/"examples/source_ingestion/ontology.json"
    receipt = {"dataset":"synthetic single high-degree ticket", "nodes_requested":args.nodes,
        "binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),
        "ontology_sha256":hashlib.sha256(schema.read_bytes()).hexdigest(),
        "capture_method":"controlling PTY -> pyte cells -> Pillow/Menlo",
        "dimensions":[[120,30],[80,24]], "languages":["en","pt-BR"],
        "default_workspace_without_view_flag":True, "graph_sample_max_rows":20,
        "driver_version":__import__("neo4j").__version__}
    with tempfile.TemporaryDirectory(prefix="brainwire-beta-tui-") as tmp:
        folder = Path(tmp).resolve()
        credentials = create(folder/"auth", "tickets", include_observer=True)
        env = {k:v for k,v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.pop("NO_COLOR", None)
        env.update(TERM="xterm-256color",BRAINWIRE_SERVER_DATA_DIR=str(folder/"data"),
            BRAINWIRE_SERVER_DEFAULT_DATABASE="tickets",BRAINWIRE_MEMORY_CACHE="64MiB",
            BRAINWIRE_MEMORY_MEMTABLES="16MiB",BRAINWIRE_MEMORY_QUERY="64MiB",
            BRAINWIRE_SERVER_MAX_MESSAGE="1MiB",BRAINWIRE_SERVER_MAX_DECODED_MESSAGE="16MiB",
            BRAINWIRE_SERVER_AUTH_FILE=str(credentials/"policy.json"),
            BRAINWIRE_BOLT_AUTH_FILE=str(credentials/"observer.json"))
        def run(*argv):
            return subprocess.run([str(binary),*argv],cwd=ROOT,env=env,check=True,capture_output=True,text=True)
        run("db","create","tickets","--ontology",str(schema))
        with socket.socket() as sock:
            sock.bind(("127.0.0.1",0))
            address="%s:%s"%sock.getsockname()
        server = None
        log_path = folder/"server.log"
        def stop():
            nonlocal server
            if server is not None:
                server.terminate()
                try:
                    server.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    server.kill();server.wait(timeout=5)
                server = None
        def start():
            nonlocal server
            with log_path.open("ab") as log:
                server=subprocess.Popen([str(binary),"serve"],env=dict(env,BRAINWIRE_SERVER_LISTEN=address),cwd=ROOT,stdout=log,stderr=log)
            for _ in range(150):
                if subprocess.run([str(binary),"probe","--address",address,"--timeout-ms","100"],env=env,capture_output=True).returncode==0:
                    return
                if server.poll() is not None:
                    raise RuntimeError("capture server exited: "+log_path.read_text())
                time.sleep(0.03)
            raise RuntimeError("capture server readiness timeout: "+log_path.read_text())
        writer = json.loads((credentials/"writer.json").read_text())
        names = []
        try:
            start()
            began=time.monotonic()
            with GraphDatabase.driver("bolt://"+address,auth=(writer["principal"],writer["credentials"])) as driver:
                with driver.session(database="tickets") as session:
                    session.run("CREATE (t:Ticket {entity_id: $id, subject: $subject, status: $status})",id="ticket-high-degree",subject="Synthetic high-degree UI fixture",status="open").consume()
                    for offset in range(0,args.nodes-1,250):
                        values=[{"entity_id":f"comment-{i:06}","ticket_entity_id":"ticket-high-degree"} for i in range(offset,min(offset+250,args.nodes-1))]
                        with session.begin_transaction() as transaction:
                            transaction.run("UNWIND $rows AS row CREATE (c:TicketComment {entity_id: row.entity_id, ticket_entity_id: row.ticket_entity_id})",rows=values).consume()
                            transaction.run("UNWIND $rows AS row MATCH (t:Ticket {entity_id: row.ticket_entity_id}), (c:TicketComment {entity_id: row.entity_id}) CREATE (t)-[:HAS_COMMENT]->(c)",rows=values).consume()
                            transaction.commit()
                    actual_nodes=session.run("MATCH (n:TicketComment) RETURN count(n) AS total").single()["total"]+session.run("MATCH (n:Ticket) RETURN count(n) AS total").single()["total"]
                    actual_edges=session.run("MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(c) AS total").single()["total"]
                    assert actual_nodes==args.nodes and actual_edges==args.nodes-1,(actual_nodes,actual_edges)
                    receipt.update(actual_nodes=actual_nodes,actual_edges=actual_edges,import_seconds=time.monotonic()-began)
            print(f"Fixture verified: {actual_nodes} nodes, {actual_edges} edges",flush=True)
            names += capture(binary,address,120,30,"en",[
                ("workspace-ontology-en-120x30",[],None,["ONTOLOGY","Ticket","NAVIGATION"]),
                ("workspace-memory-en-120x30",[b"2"],None,["MEMORY","67108864 B","REJECTIONS"]),
                ("workspace-memory-stale-en-120x30",[b"r"],stop,["STALE","67108864 B"]),
                ("workspace-memory-recovered-en-120x30",[b"r"],start,["MEMORY","67108864 B"]),
                ("workspace-indexes-en-120x30",[b"3"],None,["INDEXES","READY","ticket_entity_id"]),
                ("workspace-graph-en-120x30",[b"4"],None,["GRAPH","ticket-high-degree","20 outgoing rows / 20","MORE AVAILABLE"]),
                ("workspace-graph-scrolled-en-120x30",[b"\x1b[B"]*19,None,["> 20","comment-","MORE AVAILABLE"]),
            ],env,font)
            names += capture(binary,address,80,24,"pt-BR",[
                ("workspace-ontology-pt-BR-80x24",[],None,["ONTOLOGIA","Ticket"]),
                ("workspace-memory-pt-BR-80x24",[b"2"],None,["MEMÓRIA","67108864 B","REJEIÇÕES"]),
                ("workspace-indexes-pt-BR-80x24",[b"3"],None,["ÍNDICES","PRONTO"]),
                ("workspace-graph-pt-BR-80x24",[b"4"],None,["GRAFO","ticket-high-degree","MAIS DISPONÍVEIS"]),
            ],env,font)
            reader_env=dict(env,BRAINWIRE_BOLT_AUTH_FILE=str(credentials/"reader.json"))
            names += capture(binary,address,120,30,"en",[
                ("workspace-reader-memory-forbidden-en-120x30",[b"2"],None,["FETCH FAILED","forbidden","No sample available"]),
                ("workspace-reader-indexes-after-forbidden-en-120x30",[b"3"],None,["INDEXES","READY"]),
            ],reader_env,font)
        finally:
            stop()
        stats=json.loads(run("db","stats","tickets","--json").stdout)
        assert stats["nodes"]==args.nodes and stats["edges"]==args.nodes-1,stats
        receipt.update(captures=names,offline_stats=stats,all_child_processes_stopped=True,temporary_credentials_and_storage_removed_on_context_exit=True)
    (OUT/"receipt.json").write_text(json.dumps(receipt,indent=2)+"\n")
    print("Captured real cells; all temporary resources cleaned.",flush=True)

if __name__=="__main__":
    main()
