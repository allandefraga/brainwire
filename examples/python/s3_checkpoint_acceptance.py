"""Exercise MinIO backup and restore with a disposable container and synthetic graph."""
import argparse
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import time
import uuid
from unittest.mock import patch

import boto3
from botocore.config import Config
from botocore.exceptions import ClientError, ConnectionClosedError, EndpointConnectionError

from examples.python import checkpoint_backup, s3_checkpoint


def run(binary, image, context, entrypoint):
    binary = str(Path(binary).resolve(strict=True))
    with patch.dict(os.environ, AWS_MAX_ATTEMPTS="999", AWS_RETRY_MODE="adaptive"):
        configured = boto3.client(
            "s3", region_name="us-east-1", aws_access_key_id="fixture",
            aws_secret_access_key="fixture", config=s3_checkpoint.client_config(None),
        )
        try:
            config = configured.meta.config
            assert config.connect_timeout == 5 and config.read_timeout == 60
            assert config.retries == {"mode": "standard", "total_max_attempts": 3}
        finally:
            configured.close()
    owner = uuid.uuid4().hex
    name = f"brainwire-minio-test-{owner}"
    label = "dev.brainwire.minio-acceptance"
    user = "brainwire-test"
    password = secrets.token_hex(24)
    env = dict(os.environ, MINIO_ROOT_USER=user, MINIO_ROOT_PASSWORD=password)

    def docker(*args, **kwargs):
        command = ["docker", "--context", context, *args]
        return subprocess.run(command, capture_output=True, text=True, timeout=60, **kwargs)

    image_id = docker("image", "inspect", "--format", "{{.Id}}", image, check=True).stdout.strip()
    flags = ["--entrypoint", entrypoint] if entrypoint else []
    uncertain_create = True
    try:
        created = docker(
            "create", "--pull", "never", "--name", name, "--label", f"{label}={owner}",
            "--publish", "127.0.0.1::9000", "--memory", "384m",
            "--tmpfs", "/data:mode=0700,uid=1001,gid=1001,size=256m",
            "--user", "1001:1001", "-e", "MINIO_ROOT_USER", "-e", "MINIO_ROOT_PASSWORD",
            *flags, image_id, "server", "/data", check=True, env=env,
        )
        uncertain_create = False
        container_id = created.stdout.strip()
        docker("start", container_id, check=True)
        address = docker("port", container_id, "9000/tcp", check=True).stdout.strip()
        if not address.startswith("127.0.0.1:"):
            raise RuntimeError("unexpected MinIO port binding")
        endpoint = f"http://{address}"
        client = boto3.client(
            "s3", endpoint_url=endpoint, region_name="us-east-1",
            aws_access_key_id=user, aws_secret_access_key=password,
            config=Config(s3={"addressing_style": "path"}, proxies={}, retries={"max_attempts": 0}),
        )
        try:
            for _ in range(100):
                try:
                    client.list_buckets()
                    break
                except (ConnectionClosedError, EndpointConnectionError, ClientError):
                    time.sleep(0.1)
            else:
                logs = docker("logs", container_id)
                raise RuntimeError((logs.stdout + logs.stderr).replace(password, "[redacted]"))
            client.create_bucket(Bucket="brainwire-test")
            with tempfile.TemporaryDirectory(prefix="brainwire-s3-acceptance-") as temp:
                root = Path(temp)
                data = root / "data"
                data.mkdir()
                env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
                env["BRAINWIRE_SERVER_DATA_DIR"] = str(data)

                def graph(*args):
                    return subprocess.check_output([binary, *args], text=True, env=env, cwd=root, stderr=subprocess.PIPE)

                ontology = root / "ontology.json"
                ontology.write_text(json.dumps({
                    "version": "1",
                    "nodes": [{
                        "label": "Decision",
                        "properties": ["title", "priority"],
                        "property_types": {"priority": "integer"},
                        "indexed_fields": ["priority"],
                    }],
                    "edges": [],
                }))
                graph("db", "create", "pilot", "--ontology", str(ontology))
                graph("cypher", "--database", str(data / "pilot"), "--query",
                      "CREATE (:Decision {entity_id:'release', title:'Test backups', priority:3})")
                checkpoint = root / "snapshot"
                receipt = json.loads(graph("db", "checkpoint", "pilot", "--destination", str(checkpoint)))
                backup = root / "verified"
                checkpoint_backup.copy_checkpoint(
                    checkpoint, backup, {**receipt, "name": checkpoint.name},
                )
                cli_env = dict(
                    os.environ, AWS_ACCESS_KEY_ID=user, AWS_SECRET_ACCESS_KEY=password,
                    AWS_DEFAULT_REGION="us-east-1",
                )
                cli_env.pop("AWS_SESSION_TOKEN", None)
                cli_env.pop("AWS_SECURITY_TOKEN", None)

                def cli(*args, check=True):
                    return subprocess.run(
                        [sys.executable, "-m", "examples.python.s3_checkpoint", "--endpoint", endpoint, *args],
                        capture_output=True, text=True, env=cli_env, check=check, timeout=60,
                    )

                uri = "s3://brainwire-test/checkpoints/snapshot"
                result = json.loads(cli("upload", str(backup), uri).stdout)
                manifest_key = f"checkpoints/snapshot/_brainwire/{checkpoint_backup.MANIFEST}"
                try:
                    client.put_object(Bucket="brainwire-test", Key=manifest_key,
                                      Body=b"do not replace", IfNoneMatch="*")
                except ClientError as exc:
                    assert exc.response["ResponseMetadata"]["HTTPStatusCode"] == 412, exc
                else:
                    raise AssertionError("MinIO accepted an overwrite despite the precondition")

                restored = data / "restored"
                cli("download", uri, str(restored))
                assert checkpoint_backup.verify_copy(restored) == []
                (restored / checkpoint_backup.MANIFEST).unlink()
                checked = json.loads(graph("db", "check", "restored", "--json"))
                assert checked["status"] == "ok", checked
                rows = json.loads(graph(
                    "cypher", "--database", str(restored), "--query",
                    "MATCH (d:Decision) WHERE d.priority=3 RETURN d.title, d.priority",
                ))["rows"]
                assert rows == [["Test backups", 3]], rows
                indexes = json.loads(graph(
                    "cypher", "--database", str(restored), "--query",
                    "CALL db.indexes() YIELD label, property, state "
                    "WHERE label='Decision' AND property='priority' RETURN state",
                ))["rows"]
                assert indexes == [["scalar_equality_ready"]], indexes
                try:
                    graph(
                        "cypher", "--database", str(restored), "--query",
                        "MATCH (d:Decision {entity_id:'release'}) SET d.priority='3'",
                    )
                except subprocess.CalledProcessError as exc:
                    assert exc.stderr and "integer" in exc.stderr, exc
                else:
                    raise AssertionError("restored graph accepted an invalid property type")
                current = (backup / "CURRENT").read_bytes()
                client.put_object(Bucket="brainwire-test", Key="checkpoints/snapshot/_brainwire/CURRENT",
                                  Body=b"x" * len(current))
                rejected = cli("download", uri, str(data / "rejected"), check=False)
                assert rejected.returncode != 0 and "checksum mismatch" in rejected.stderr, rejected
                assert not (data / "rejected").exists() and not (data / "rejected.partial").exists()
                print(json.dumps({"status": "ok", "image": image_id, "files": result["files"],
                                  "rows": rows, "overwrite_refused": True, "corruption_refused": True,
                                  "invalid_type_refused": True, "index_state": indexes[0][0]}))
        finally:
            client.close()
    finally:
        deadline = time.monotonic() + (5 if uncertain_create else 0)
        while True:
            found = docker(
                "container", "ls", "--all", "--no-trunc", "--filter", f"name=^/{name}$",
                "--format", "{{.ID}}", check=True,
            ).stdout.splitlines()
            if found:
                (found_id,) = found
                identity = docker(
                    "container", "inspect", "--format", '{{.Name}} {{json .Config.Labels}}',
                    found_id, check=True,
                ).stdout.strip()
                found_name, labels = identity.split(" ", 1)
                if found_name != f"/{name}" or json.loads(labels).get(label) != owner:
                    raise RuntimeError("refusing removal of container with unexpected ownership")
                docker("rm", "--force", "--volumes", found_id, check=True)
                break
            if time.monotonic() >= deadline:
                break
            time.sleep(0.25)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--image", required=True, help="local MinIO image or image ID")
    parser.add_argument("--docker-context", default="default")
    parser.add_argument("--minio-entrypoint", help="absolute MinIO executable path for wrapped images")
    args = parser.parse_args()
    run(args.binary, args.image, args.docker_context, args.minio_entrypoint)
