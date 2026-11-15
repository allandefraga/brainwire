"""Run the DuckLake CLI and its spawned writers against a private TLS server.

Requires OpenSSL, the Bolt driver, and already installed DuckLake and SQLite
extensions. All database files, certificates and credentials are temporary.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

import duckdb
from neo4j import GraphDatabase, TrustCustomCAs

from examples.python.create_service_tokens import create
from examples.python.duckdb_acceptance import server


ROOT = Path(__file__).resolve().parents[2]
ROWS = 20


def certificates(root):
    def openssl(*args):
        subprocess.run(["openssl", *args], cwd=root, check=True, capture_output=True, timeout=10)

    (root / "ca.cnf").write_text(
        "[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n"
        "[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
    )
    for name in ("ca", "wrong-ca"):
        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", "ca.cnf",
                "-subj", f"/CN=Synthetic {name}", "-keyout", f"{name}.key", "-out", f"{name}.crt")
    openssl("req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=127.0.0.1",
            "-keyout", "server.key", "-out", "server.csr")
    (root / "leaf.cnf").write_text(
        "subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n"
    )
    openssl("x509", "-req", "-in", "server.csr", "-CA", "ca.crt", "-CAkey", "ca.key",
            "-CAcreateserial", "-days", "1", "-extfile", "leaf.cnf", "-out", "server.crt")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-ducklake-tls-") as temporary:
        root = Path(temporary)
        certificates(root)
        auth = create(root / "auth", "lake")
        writer_file = auth / "writer.json"
        writer = json.loads(writer_file.read_text())
        reader = json.loads((auth / "reader.json").read_text())
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{sock.getsockname()[1]}"
        uri = f"bolt://{address}"
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_LISTEN=address, BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
                   BRAINWIRE_SERVER_DEFAULT_DATABASE="lake", BRAINWIRE_SERVER_AUTH_FILE=str(auth / "policy.json"),
                   BRAINWIRE_BOLT_AUTH_FILE=str(writer_file), BRAINWIRE_SERVER_TLS_CERT_FILE=str(root / "server.crt"),
                   BRAINWIRE_SERVER_TLS_KEY_FILE=str(root / "server.key"))
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [
            {"label": "Customer", "properties": ["name"]},
            {"label": "ImportCheckpoint", "properties": ["snapshot", "lease", "contract_fingerprint"]},
        ]}))
        subprocess.run([binary, "db", "create", "lake", "--ontology", ontology],
                       env=env, check=True, capture_output=True, timeout=15)

        lake = f"ducklake:sqlite:{root / 'catalog.sqlite'}"
        with duckdb.connect() as source:
            source.execute("LOAD ducklake")
            source.execute("LOAD sqlite")
            source.execute(f"ATTACH '{lake}' AS fixture (DATA_PATH '{root / 'lake'}/')")
            source.execute("CREATE TABLE fixture.customers(id VARCHAR, name VARCHAR)")
            source.execute("INSERT INTO fixture.customers SELECT 'c' || i, 'name' || i FROM range(?) t(i)", [ROWS])
        mapping = root / "mapping.json"
        mapping.write_text(json.dumps({"table": "main.customers", "label": "Customer", "id": "id",
                                       "properties": {"name": "name"}}))
        command = [sys.executable, "-m", "examples.python.ducklake_sync", str(mapping), "--lake", lake,
                   "--database", "lake", "--source-id", "test", "--source-identity", "fixture",
                   "--workers", "2", "--uri", uri, "--tls-ca"]
        with GraphDatabase.driver(uri, auth=(reader["principal"], reader["credentials"]), encrypted=True,
                                  trusted_certificates=TrustCustomCAs(str(root / "ca.crt")),
                                  connection_timeout=2, max_transaction_retry_time=0) as driver, \
                server(binary, root, env, driver), driver.session(database="lake") as session:
            rejected = subprocess.run(command + [str(root / "wrong-ca.crt")], cwd=ROOT, env=env,
                                      timeout=20, capture_output=True, text=True)
            assert writer["credentials"] not in rejected.stdout + rejected.stderr, "CLI exposed its token"
            assert rejected.returncode and "SSLCertVerificationError" in rejected.stderr, rejected.stderr
            query = "MATCH (c:Customer) RETURN count(c) AS n"
            assert session.run(query).single()["n"] == 0
            accepted = subprocess.run(command + [str(root / "ca.crt")], cwd=ROOT, env=env,
                                      timeout=30, capture_output=True, text=True)
            assert writer["credentials"] not in accepted.stdout + accepted.stderr, "CLI exposed its token"
            assert accepted.returncode == 0, accepted.stderr
            report = json.loads(accepted.stdout)
            assert report["workers"] == 2 and report["upserted"] == ROWS, report
            assert session.run(query).single()["n"] == ROWS
    return {"result": "PASS", "workers": 2, "rows": ROWS, "wrong_ca": "rejected"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    print(json.dumps(run(str(parser.parse_args().binary.resolve(strict=True)))))
