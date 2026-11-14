"""Verify the vault importer against a disposable local Brainwire server."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from neo4j.exceptions import Neo4jError

from brainwire import connect
from examples.obsidian.import_vault import CREATE_NOTES, MAX_NOTES, MAX_REFERENCES, import_notes, prepare


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-vault-test-") as folder:
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        address = f"127.0.0.1:{port}"
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=folder, BRAINWIRE_SERVER_LISTEN=address,
                   BRAINWIRE_SERVER_DEFAULT_DATABASE="vault", BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="4MiB", BRAINWIRE_MEMORY_QUERY="32MiB")
        for database in ("vault", "capacity", "capacity_rollback"):
            subprocess.run([binary, "db", "create", database, "--ontology", str(HERE / "ontology.json")],
                           check=True, env=env, capture_output=True)
        with (Path(folder) / "server.log").open("w") as log:
            server = subprocess.Popen([binary, "serve"], env=env, stdout=log, stderr=log)
            try:
                for _ in range(200):
                    if server.poll() is not None:
                        raise RuntimeError("vault test server stopped during startup")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.025)
                else:
                    raise RuntimeError("vault test server did not start")
                uri = f"bolt://{address}"
                test_graph(uri, env)
                test_capacity(uri, Path(folder))
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)


def test_graph(uri, env):
    notes, links = prepare(HERE.resolve() / "vault")
    with connect(uri, "vault", auth=None) as graph:
        # An edge count mismatch must also roll back note writes.
        try:
            import_notes(graph, notes, [*links, {"source": "missing", "target": notes[0]["path"]}])
        except ValueError as error:
            assert "creation count" in str(error)
        else:
            raise AssertionError("missing endpoint unexpectedly accepted")
        assert graph.run_cypher("MATCH (n) RETURN n LIMIT 1") == []

        # Two snapshots can each see an empty graph. A racing CREATE must conflict.
        with graph.driver.session(database="vault") as first, graph.driver.session(database="vault") as second:
            with first.begin_transaction() as tx:
                assert tx.run("MATCH (n) RETURN n LIMIT 1").single() is None
                tx.run(CREATE_NOTES, rows=notes[:1]).consume()
                second.run(CREATE_NOTES, rows=[{**notes[0], "body": "competing writer"}]).consume()
                try:
                    tx.commit()
                except Neo4jError as error:
                    assert error.code == "Neo.TransientError.Transaction.Outdated", error
                else:
                    raise AssertionError("concurrent node creation overwrote a committed note")
        assert graph.run_cypher("MATCH (n:Note) RETURN n.body AS body") == [{"body": "competing writer"}]
        # Reset our own disposable test data for the end-to-end CLI flow.
        graph.run_cypher("MATCH (n:Note) DELETE n")

        command = [sys.executable, "-m", "examples.obsidian.import_vault", str(HERE.resolve() / "vault"),
                   "--database", "vault", "--uri", uri]
        loaded = subprocess.run(command, cwd=ROOT, env=env, check=True, text=True, capture_output=True)
        assert json.loads(loaded.stdout) == {"notes": 2, "links": 2, "written": True}
        query = ("MATCH (a:Note)-[:LINKS_TO]->(b:Note) "
                 "RETURN a.entity_id AS source, b.entity_id AS target ORDER BY source")
        assert graph.run_cypher(query) == links
        repeated = subprocess.run(command, cwd=ROOT, env=env, text=True, capture_output=True)
        assert repeated.returncode == 2 and "must be empty" in repeated.stderr
        assert graph.run_cypher(query) == links
    print("PASS: vault CLI, links, transactional rollback, competing CREATE and repeat refusal")


def test_capacity(uri, root):
    vault = root.resolve() / "max-notes"
    vault.mkdir()
    for index in range(MAX_NOTES):
        # Exercise exactly 512 distinct relationships and 256 KiB of Markdown.
        references = " ".join(f"[[n{(index + offset) % MAX_NOTES}]]" for offset in range(1, 5))
        (vault / f"n{index}.md").write_text(references.ljust(2048), encoding="utf-8")
    notes, links = prepare(vault)
    assert len(notes) == MAX_NOTES and len(links) == MAX_REFERENCES
    with connect(uri, "capacity_rollback", auth=None) as graph:
        try:
            import_notes(graph, notes, [*links[:-1], {**links[-1], "target": "missing"}])
        except ValueError as error:
            assert "creation count" in str(error)
        else:
            raise AssertionError("final batch unexpectedly accepted a missing endpoint")
        assert graph.run_cypher("MATCH (n) RETURN count(n) AS nodes") == [{"nodes": 0}]
    with connect(uri, "capacity", auth=None) as graph:
        import_notes(graph, notes, links)
        assert graph.run_cypher("MATCH (n:Note) RETURN count(n) AS nodes") == [{"nodes": MAX_NOTES}]
        query = "MATCH (:Note)-[r:LINKS_TO]->(:Note) RETURN count(r) AS links"
        assert graph.run_cypher(query) == [{"links": MAX_REFERENCES}]
    print("PASS: 128 notes, 512 relationships, 256 KiB of Markdown and rollback in the final batch")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    run(str(parser.parse_args().binary.resolve()))
