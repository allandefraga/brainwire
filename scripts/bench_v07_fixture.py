"""Prepare a private, disposable Bolt ACL fixture from a synthetic baseline receipt.

The source graph is copied under an exclusive lock and is never modified. The
prepared server is stopped and checked before its read-only manifest is frozen.
"""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import subprocess
import time

LABELS = {"Usuario": "usr", "Carro": "car", "Endereco": "end", "Casa": "cas"}
EDGES = [("Usuario", "TEM_CARRO", "Carro"), ("Usuario", "TEM_ENDERECO", "Endereco"),
         ("Usuario", "TEM_CASA", "Casa"), ("Casa", "CASA_FICA_EM", "Endereco")]
BRANDS = ["MarcaA", "MarcaB", "MarcaC", "MarcaD"]
READER = "user:benchmark:reader"
HIDDEN = "user:benchmark:hidden"
SCHEMA = "CALL db.schema() YIELD version, nodes, relationships RETURN version, nodes, relationships"


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def private_json(path, value):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def replace_private_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    private_json(temporary, value)
    temporary.replace(path)


def source_fixture(receipt_path, confirmed):
    if not confirmed:
        raise ValueError("--confirm-source-synthetic is required")
    receipt_path = Path(receipt_path)
    if receipt_path.is_symlink():
        raise ValueError("source receipt must not be a symbolic link")
    receipt_path = receipt_path.resolve(strict=True)
    receipt = json.loads(receipt_path.read_bytes())
    dataset = receipt.get("dataset", {})
    count = dataset.get("rows_per_label")
    if (receipt.get("kind") != "embedded synthetic benchmark"
            or receipt.get("status") != "completed_embedded_only"
            or type(count) is not int or not 1 <= count <= 1_000_000
            or dataset.get("fixture_nodes") != 4 * count or dataset.get("fixture_edges") != 4 * count
            or dataset.get("labels") != list(LABELS)):
        raise ValueError("source must be a successful, recognized synthetic baseline receipt")
    phases = receipt.get("phases", [])
    if not any(p.get("name") == "check" and p.get("status") == "ok"
               and {"phase": "check", "status": "ok"} in p.get("records", []) for p in phases):
        raise ValueError("source receipt must include an invariant check with status=ok")
    graph = receipt_path.parent / "data" / "bench"
    if not graph.is_dir() or graph.is_symlink() or graph.parent.is_symlink():
        raise ValueError("source receipt must own a real data/bench directory")
    if not (graph / "LOCK").is_file():
        raise ValueError("source RocksDB LOCK file is missing")
    for item in graph.rglob("*"):
        if item.is_symlink() or not (item.is_file() or item.is_dir()):
            raise ValueError("source graph contains an unsupported file or symbolic link")
    return graph, count, sha256(receipt_path)


@contextmanager
def locked_copy(source, destination):
    # RocksDB's POSIX lock prevents copying a graph owned by a live server.
    with (source / "LOCK").open("r+b") as lock:
        try:
            fcntl.lockf(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise ValueError("source graph is locked; stop its owner before copying") from error
        if os.fstat(lock.fileno()).st_nlink != 1:
            raise ValueError("source LOCK must not have hard-link aliases")
        # Closing any descriptor for the same inode releases this process's
        # POSIX locks. Never let copytree open/close the source LOCK file.
        shutil.copytree(source, destination,
                        ignore=lambda directory, _names: {"LOCK"} if Path(directory) == source else set())
        descriptor = os.open(destination / "LOCK", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(descriptor)
        yield


def expected_results(count, acl_mode="balanced"):
    if acl_mode not in ("balanced", "full-cursor"):
        raise ValueError("unknown ACL mode")
    visible = sum(count // 8 + int(count % 8 > residue) for residue in range(4))
    def top(restricted):
        rows = [{"group_value": brand, "count": (count // 8 + int(count % 8 > i)) if restricted
                 else (count // 4 + int(count % 4 > i))} for i, brand in enumerate(BRANDS)]
        return sorted([row for row in rows if row["count"]], key=lambda row: (-row["count"], row["group_value"]))
    return {"allow_all": {"count_pairs": count, "cursor_rows": count, "top_n": top(False)},
            "restricted": {"count_pairs": visible, "cursor_rows": count if acl_mode == "full-cursor" else visible,
                           "top_n": top(True)}}


def credentials(database):
    tokens = {name: secrets.token_hex(32) for name in ("fixture-admin", "fixture-reader")}
    principals = [dict(id="fixture-admin", token_sha256=hashlib.sha256(tokens["fixture-admin"].encode()).hexdigest(),
                       graphs={database: "write"}, instance_admin=True),
                  dict(id="fixture-reader", token_sha256=hashlib.sha256(tokens["fixture-reader"].encode()).hexdigest(),
                       graphs={database: "read"}, visibility_delegate=True, instance_admin=False)]
    return tokens, principals


def query(driver, database, cypher, params=None, policy=None, write=False, timeout=300):
    with driver.session(database=database, default_access_mode="WRITE" if write else "READ") as session:
        metadata = {} if policy is None else {"brainwire_visibility": policy}
        with session.begin_transaction(metadata=metadata, timeout=timeout) as tx:
            rows = tx.run(cypher, {} if params is None else params).data()
            tx.commit()
    return rows


def stamp(driver, database, count, batch_size, acl_mode="balanced"):
    if acl_mode not in ("balanced", "full-cursor"):
        raise ValueError("unknown ACL mode")
    total = 0
    for label in ("Usuario", "Carro"):
        for offset in range(0, count, batch_size):
            entries = [{"label": label, "entityId": f"{LABELS[label]}-{i:08}",
                        "allow": [READER if (i % 8 < 4 or (label == "Usuario" and acl_mode == "full-cursor")) else HIDDEN], "deny": [],
                        "connectorIds": [], "sources": ["synthetic-v07"]}
                       for i in range(offset, min(offset + batch_size, count))]
            rows = query(driver, database, "CALL db.setNodeAcls($rows) YIELD updated RETURN updated",
                         {"rows": entries}, write=True)
            if rows != [{"updated": len(entries)}]:
                raise ValueError("ACL batch acknowledgment differs from requested rows")
            total += len(entries)
    return total


def validate_base(driver, database, count):
    rows = query(driver, database, SCHEMA)
    if len(rows) != 1 or type(rows[0].get("version")) is not int or rows[0]["version"] < 1:
        raise ValueError("schema must return a positive integer version")
    schema = rows[0]
    if {n.get("label") for n in schema["nodes"]} != set(LABELS) | {"Pessoa"}:
        raise ValueError("source does not have the recognized baseline labels")
    signatures = {(e.get("source"), e.get("name"), e.get("target")) for e in schema["relationships"]}
    if signatures != set(EDGES):
        raise ValueError("source does not have the recognized baseline relationships")
    for label in LABELS:
        if query(driver, database, f"MATCH (n:{label}) RETURN count(n) AS count") != [{"count": count}]:
            raise ValueError("source label cardinality differs from receipt")
    for source, edge, target in EDGES:
        if query(driver, database, f"MATCH (a:{source})-[:{edge}]->(b:{target}) RETURN count(*) AS count") != [{"count": count}]:
            raise ValueError("source relationship cardinality differs from receipt")
    total = query(driver, database, "MATCH (n) RETURN count(n) AS count")
    if len(total) != 1 or type(total[0].get("count")) is not int or total[0]["count"] < 4 * count:
        raise ValueError("source total node count is inconsistent")
    return schema["version"], total[0]["count"] - 4 * count


def validate_visibility(driver, database, expected):
    policies = {"allow_all": {"allow_all": True}, "restricted": {"principals": [READER]}}
    for mode, policy in policies.items():
        actual = {
            "count_pairs": query(driver, database, "MATCH (a:Usuario)-[:TEM_CARRO]->(b:Carro) RETURN count(*) AS count", policy=policy),
            "cursor_rows": query(driver, database, "MATCH (n:Usuario) RETURN count(n) AS count", policy=policy),
            "top_n": query(driver, database, "MATCH (a:Usuario)-[:TEM_CARRO]->(b:Carro) RETURN b.marca_teste AS group_value, count(*) AS count ORDER BY count DESC, group_value LIMIT 5", policy=policy),
        }
        if (actual["count_pairs"] != [{"count": expected[mode]["count_pairs"]}]
                or actual["cursor_rows"] != [{"count": expected[mode]["cursor_rows"]}]
                or actual["top_n"] != expected[mode]["top_n"]):
            raise ValueError("verified visibility results differ from synthetic fixture")


class Server:
    def __init__(self, binary, env, directory):
        descriptor = os.open(directory / "server.log", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        self.log = os.fdopen(descriptor, "ab")
        try:
            self.process = subprocess.Popen([str(binary), "serve"], cwd=directory, env=env,
                                            stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log)
        except BaseException:
            self.log.close()
            raise
    def poll(self):
        return self.process.poll()
    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.log.close()


def runtime_env(settings):
    return {**{k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}, **settings}


def wait_ready(driver, database, server, policy=None):
    for _ in range(150):
        if server.poll() is not None:
            raise RuntimeError("fixture server exited before readiness")
        try:
            if query(driver, database, "RETURN 1 AS ready", policy=policy, timeout=1) == [{"ready": 1}]:
                return
        except Exception:
            pass
        time.sleep(0.1)
    raise RuntimeError("fixture server readiness deadline exceeded")


def offline_check(binary, env, directory):
    result = subprocess.run([str(binary), "db", "check", "bench", "--json"], cwd=directory,
                            env=env, capture_output=True, text=True, timeout=600)
    if result.returncode != 0:
        raise RuntimeError("offline consistency check failed")
    record = json.loads(result.stdout)
    if record.get("status") != "ok":
        raise RuntimeError("offline consistency check did not report status=ok")
    return record


def loopback_address():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return "%s:%s" % sock.getsockname()


def prepare(args, driver_factory=None, server_factory=Server, checker=offline_check,
            address_factory=loopback_address):
    source, count, receipt_hash = source_fixture(args.source_receipt, args.confirm_source_synthetic)
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_revision):
        raise ValueError("source revision must be a complete lowercase Git commit")
    binary = Path(args.binary).resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError("binary must be executable")
    output = Path(args.output).resolve()
    if output == source or source in output.parents or output in source.parents:
        raise ValueError("output must be separate from source graph")
    if not 1 <= args.batch_size <= 10000:
        raise ValueError("batch size must be between 1 and 10000")
    if args.acl_mode not in ("balanced", "full-cursor"):
        raise ValueError("unknown ACL mode")
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    (output / "data").mkdir(mode=0o700)
    receipt = {"kind": "synthetic Bolt ACL fixture preparation", "status": "running",
               "source_receipt_sha256": receipt_hash, "rows_per_label": count, "acl_mode": args.acl_mode,
               "source_revision": args.source_revision, "server_binary_sha256": sha256(binary),
               "qualification_gaps": ["Caller confirms source synthetic/disposable; receipt identity is not a cryptographic data provenance guarantee.",
                    "Additional Pessoa write-fixture nodes are retained and counted explicitly.",
                    "Preparation is not a performance run or a hard memory-cap qualification.",
                    "ACL stamping covers Usuario and Carro only; other label ACLs are not used by this diagnostic."]}
    path = output / "prepared.json"
    private_json(path, receipt)
    server = None
    try:
        with locked_copy(source, output / "data" / "bench"):
            pass
        (output / "auth").mkdir(mode=0o700)
        tokens, principals = credentials("bench")
        policy_path = output / "auth" / "policy.json"
        private_json(policy_path, {"principals": principals})
        address = address_factory()
        settings = {"BRAINWIRE_SERVER_DATA_DIR": str(output / "data"), "BRAINWIRE_SERVER_DEFAULT_DATABASE": "bench",
                    "BRAINWIRE_SERVER_LISTEN": address, "BRAINWIRE_SERVER_AUTH_FILE": str(policy_path),
                    "BRAINWIRE_MEMORY_CACHE": "1GiB", "BRAINWIRE_MEMORY_MEMTABLES": "256MiB", "BRAINWIRE_MEMORY_QUERY": "2GiB",
                    "BRAINWIRE_SERVER_QUERY_TIMEOUT_MS": "300000", "BRAINWIRE_SERVER_MAX_MESSAGE": "16MiB",
                    "BRAINWIRE_SERVER_MAX_DECODED_MESSAGE": "64MiB"}
        env = runtime_env(settings)
        if driver_factory is None:
            from neo4j import GraphDatabase
            driver_factory = GraphDatabase.driver
        with driver_factory("bolt://" + address, auth=("fixture-admin", tokens["fixture-admin"]),
                            max_transaction_retry_time=0, connection_timeout=1, max_connection_pool_size=1) as driver:
            server = server_factory(binary, env, output)
            wait_ready(driver, "bench", server)
            version, additional = validate_base(driver, "bench", count)
            receipt["additional_nodes_outside_four_label_fixture"] = additional
            receipt["stamped_acl_nodes"] = stamp(driver, "bench", count, args.batch_size, args.acl_mode)
        expected = expected_results(count, args.acl_mode)
        with driver_factory("bolt://" + address, auth=("fixture-reader", tokens["fixture-reader"]),
                            max_transaction_retry_time=0, connection_timeout=1, max_connection_pool_size=1) as driver:
            validate_visibility(driver, "bench", expected)
        server.stop()
        server = None
        receipt["db_check"] = checker(binary, env, output)
        # Only the delegated reader remains when this fixture is restarted for measurement.
        replace_private_json(policy_path, {"principals": [principals[1]]})
        dataset_id = "synthetic-v07-" + secrets.token_hex(8)
        manifest = {"synthetic": True, "disposable": True, "frozen": True,
                    "dataset_id": dataset_id, "server_id": "isolated-" + secrets.token_hex(8), "database": "bench",
                    "schema_version": version, "source_revision": args.source_revision,
                    "server_binary_sha256": receipt["server_binary_sha256"],
                    "schema": {"source_label": "Usuario", "target_label": "Carro", "relationship_type": "TEM_CARRO",
                               "group_property": "marca_teste", "cursor_label": "Usuario"},
                    "top_n": 5, "fixture_acl_mode": args.acl_mode, "expected": expected}
        private_json(output / "manifest.json", manifest)
        private_json(output / "server.json", {"binary": str(binary), "environment": settings})
        private_json(output / "client-env.json", {"BRAINWIRE_BENCH_URI": "bolt://" + address,
                     "BRAINWIRE_BENCH_PRINCIPAL": "fixture-reader", "BRAINWIRE_BENCH_TOKEN": tokens["fixture-reader"],
                     "BRAINWIRE_BENCH_RESTRICTED_VISIBILITY": json.dumps({"principals": [READER]}),
                     "BRAINWIRE_BENCH_CONFIRM_SYNTHETIC": dataset_id})
        receipt.update(status="prepared_read_only_server_stopped", expected=expected, schema_version=version)
    except BaseException as error:
        (output / "manifest.json").unlink(missing_ok=True)
        receipt.update(status="interrupted" if isinstance(error, KeyboardInterrupt) else "failed",
                       error={"type": type(error).__name__, "code": getattr(error, "code", None)})
        raise
    finally:
        if server is not None:
            server.stop()
        replace_private_json(path, receipt)
    print(output)
    return 0


def measurement_server(folder):
    folder = Path(folder).resolve(strict=True)
    receipt = json.loads((folder / "prepared.json").read_text())
    config = json.loads((folder / "server.json").read_text())
    if receipt.get("status") != "prepared_read_only_server_stopped":
        raise ValueError("fixture preparation did not complete")
    binary = Path(config["binary"])
    if sha256(binary) != receipt["server_binary_sha256"]:
        raise ValueError("fixture binary changed; prepare with the intended artifact")
    settings = config["environment"]
    if (settings.get("BRAINWIRE_SERVER_DATA_DIR") != str(folder / "data")
            or settings.get("BRAINWIRE_SERVER_DEFAULT_DATABASE") != "bench"
            or settings.get("BRAINWIRE_SERVER_AUTH_FILE") != str(folder / "auth" / "policy.json")
            or not re.fullmatch(r"127\.0\.0\.1:[1-9][0-9]{0,4}", settings.get("BRAINWIRE_SERVER_LISTEN", ""))
            or int(settings["BRAINWIRE_SERVER_LISTEN"].split(":")[1]) > 65535):
        raise ValueError("server settings must retain the isolated graph and loopback listener")
    graph = folder / "data" / "bench"
    if (folder / "data").is_symlink() or graph.is_symlink() or not graph.is_dir():
        raise ValueError("measurement data must remain in the prepared directory")
    policy = json.loads(Path(settings["BRAINWIRE_SERVER_AUTH_FILE"]).read_text())
    if len(policy.get("principals", [])) != 1 or policy["principals"][0].get("graphs") != {"bench": "read"}:
        raise ValueError("measurement policy must contain only the graph reader")
    principal = policy["principals"][0]
    if principal.get("instance_admin") is not False or principal.get("visibility_delegate") is not True:
        raise ValueError("measurement principal must be a non-admin visibility delegate")
    return binary, settings


def serve(args):
    folder = Path(args.fixture).resolve(strict=True)
    binary, settings = measurement_server(folder)
    # Replace the wrapper so stop signals reach the database process directly.
    os.chdir(folder)
    os.execve(str(binary), [str(binary), "serve"], runtime_env(settings))


def check(args):
    folder = Path(args.fixture).resolve(strict=True)
    binary, settings = measurement_server(folder)
    target = folder / "post-benchmark-check.json"
    if target.exists():
        raise ValueError("post-benchmark check receipt already exists")
    record = offline_check(binary, runtime_env(settings), folder)
    private_json(target, record)
    print(target)
    return 0


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    prep = commands.add_parser("prepare", help="copy and stamp a recognized disposable synthetic baseline")
    prep.add_argument("--source-receipt", type=Path, required=True)
    prep.add_argument("--confirm-source-synthetic", action="store_true", required=True)
    prep.add_argument("--binary", type=Path, required=True)
    prep.add_argument("--source-revision", required=True)
    prep.add_argument("--output", type=Path, required=True, help="new private output directory")
    prep.add_argument("--batch-size", type=int, default=2000)
    prep.add_argument("--acl-mode", choices=("balanced", "full-cursor"), default="balanced",
                      help="balanced hides half of both labels; full-cursor keeps every Usuario visible")
    run = commands.add_parser("serve", help="run only the prepared read-only server until interrupted")
    run.add_argument("--fixture", type=Path, required=True)
    verify = commands.add_parser("check", help="check only the prepared graph after stopping its server")
    verify.add_argument("--fixture", type=Path, required=True)
    return result


def main():
    args = parser().parse_args()
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        if args.command == "prepare": return prepare(args)
        if args.command == "serve": return serve(args)
        return check(args)
    except KeyboardInterrupt:
        return 130
    except Exception as error:
        raise SystemExit(type(error).__name__ + "; inspect private prepared.json and server.log") from None


if __name__ == "__main__":
    raise SystemExit(main())
