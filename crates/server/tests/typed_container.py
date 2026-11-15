"""Exercise typed graph recovery and the old-image format guard on Linux.

Requires Python 3.9+, a working Docker context, and both images loaded locally.
Uses a fresh named volume and an isolated container with no network access.
"""

import argparse
import json
import subprocess
import time
import uuid


ONTOLOGY = {
    "version": "1",
    "nodes": [
        {
            "label": "Project",
            "properties": ["priority"],
            "indexed_fields": ["priority"],
            "property_types": {"priority": "integer"},
        },
        {"label": "Decision", "properties": ["approved"]},
    ],
    "edges": [
        {
            "type": "HAS_DECISION",
            "from": {"label": "Project"},
            "to": {"label": "Decision"},
            "properties": ["confidence"],
            "property_types": {"confidence": "float"},
        }
    ],
}

READ_GRAPH = (
    "MATCH (p:Project)-[r:HAS_DECISION]->(d:Decision) "
    "WHERE p.priority=2 RETURN p.entity_id, r.confidence, d.approved"
)
EXPECTED_ROWS = [["p", 0.75, True]]
BAD_NODE = (
    "CREATE (:Project {entity_id:'rolled-back', priority:2}) "
    "CREATE (:Project {entity_id:'bad', priority:'2'})"
)
BAD_EDGE = "MATCH (:Project)-[r:HAS_DECISION]->(:Decision) SET r.confidence=1"
LABEL_KEY = "dev.brainwire.typed-container-test"


def docker(*args, input_text=None, error_contains=()):
    result = subprocess.run(
        ["docker", *args],
        input=input_text,
        capture_output=True,
        text=True,
        timeout=60,
    )
    output = result.stdout + result.stderr
    if error_contains:
        if result.returncode and all(part.casefold() in output.casefold() for part in error_contains):
            return output.strip()
    elif result.returncode == 0:
        return result.stdout.strip()
    raise RuntimeError(f"docker {args!r} exited {result.returncode}: {output.strip()}")


def image_config(ref):
    (image,) = json.loads(docker("image", "inspect", ref))
    if image["Os"] != "linux":
        raise RuntimeError(f"Expected a Linux image: {ref}")
    return {key: image[key] for key in ("Id", "Os", "Architecture")}


def verify_rows(output, expected):
    actual = json.loads(output)["rows"]
    if json.dumps(actual) != json.dumps(expected):
        raise RuntimeError(f"Expected rows {expected!r}, received {actual!r}")


def wait_for_ready(container):
    deadline = time.monotonic() + 30
    while True:
        try:
            docker("exec", container, "brainwire", "probe", "--mode", "ready")
            return
        except RuntimeError as exc:
            if time.monotonic() >= deadline:
                raise RuntimeError(docker("logs", container)) from exc
            time.sleep(0.25)


def test_images(candidate, previous):
    candidate_id = candidate["Id"]
    previous_id = previous["Id"]
    if candidate_id == previous_id:
        raise RuntimeError("The candidate and previous images resolve to the same ID")
    if candidate["Architecture"] != previous["Architecture"]:
        raise RuntimeError("Candidate and previous image architectures differ")

    run_id = uuid.uuid4().hex
    label = f"{LABEL_KEY}={run_id}"
    volume = f"brainwire-typed-{run_id}"
    server = f"{volume}-server"
    run_args = (
        "--pull=never",
        "--network=none",
        "--label", label,
        "--mount", f"type=volume,source={volume},target=/data",
        "-e", "BRAINWIRE_SERVER_DATA_DIR=/data",
        "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=typed",
        "-e", "BRAINWIRE_SERVER_LISTEN=127.0.0.1:7687",
        "-e", "BRAINWIRE_MEMORY_CACHE=64MiB",
        "-e", "BRAINWIRE_MEMORY_MEMTABLES=8MiB",
    )

    def run(image, *args, options=(), input_text=None, error_contains=()):
        return docker(
            "run", "--rm", *run_args, *options, image, *args,
            input_text=input_text, error_contains=error_contains,
        )

    def cypher(image, graph, query, *, error_contains=()):
        return run(
            image, "cypher", "--database", f"/data/{graph}", "--query", query,
            error_contains=error_contains,
        )

    docker("volume", "inspect", volume, error_contains=("no such volume",))
    docker("container", "inspect", server, error_contains=("no such container",))
    try:
        docker("volume", "create", "--label", label, volume)
        (created_volume,) = json.loads(docker("volume", "inspect", volume))
        if (created_volume.get("Labels") or {}).get(LABEL_KEY) != run_id:
            raise RuntimeError(f"Unexpected ownership label for volume {volume}")
        run(
            candidate_id, "-ec", "cat > /data/.typed-ontology.json",
            options=("-i", "--entrypoint", "/bin/sh"), input_text=json.dumps(ONTOLOGY),
        )
        run(candidate_id, "db", "create", "typed", "--ontology", "/data/.typed-ontology.json")
        run(candidate_id, "db", "create", "untyped")
        cypher(candidate_id, "untyped", "CALL db.createLabel('Project')")
        cypher(
            candidate_id, "untyped",
            "CREATE (:Project {entity_id:'ordinary', priority:'free-form'})",
        )
        cypher(
            candidate_id, "typed",
            "CREATE (:Project {entity_id:'p', priority:1}) "
            "CREATE (:Decision {entity_id:'d', approved:true})",
        )
        cypher(
            candidate_id, "typed",
            "MATCH (p:Project {entity_id:'p'}), (d:Decision {entity_id:'d'}) "
            "MERGE (p)-[:HAS_DECISION {confidence:0.75}]->(d) SET p.priority=2",
        )
        cypher(candidate_id, "typed", BAD_NODE, error_contains=("priority", "integer"))
        cypher(candidate_id, "typed", BAD_EDGE, error_contains=("confidence", "float"))
        verify_rows(cypher(
            candidate_id, "typed", "MATCH (p:Project) RETURN count(p)"
        ), [[1]])
        verify_rows(cypher(candidate_id, "typed", READ_GRAPH), EXPECTED_ROWS)

        docker("run", "-d", "--name", server, *run_args, candidate_id, "serve")
        wait_for_ready(server)
        first_start = docker("inspect", "--format", "{{.State.StartedAt}}", server)
        docker("restart", "--time", "10", server)
        wait_for_ready(server)
        second_start = docker("inspect", "--format", "{{.State.StartedAt}}", server)
        if first_start == second_start:
            raise RuntimeError("Container start timestamp did not change")
        docker("stop", "--time", "10", server)
        verify_rows(cypher(candidate_id, "typed", READ_GRAPH), EXPECTED_ROWS)

        run(candidate_id, "db", "checkpoint", "typed", "--destination", "/data/.snapshot")
        run(
            candidate_id, "-ec",
            "mkdir -m 700 /data/restored && cp -a /data/.snapshot/. /data/restored/",
            options=("--entrypoint", "/bin/sh"),
        )
        run(candidate_id, "db", "check", "restored")
        verify_rows(cypher(candidate_id, "restored", READ_GRAPH), EXPECTED_ROWS)
        cypher(candidate_id, "restored", BAD_EDGE, error_contains=("confidence", "float"))

        verify_rows(cypher(
            previous_id, "untyped", "MATCH (p:Project) RETURN p.priority"
        ), [["free-form"]])
        cypher(previous_id, "untyped", "MATCH (p:Project) SET p.priority='old-image-write'")
        verify_rows(cypher(
            candidate_id, "untyped", "MATCH (p:Project) RETURN p.priority"
        ), [["old-image-write"]])
        refused = {}
        for target in ("typed", ".snapshot", "restored"):
            refused[target] = cypher(
                previous_id, target, "RETURN 1",
                error_contains=("column famil", "typed_properties_v1"),
            )
        return {
            "candidate": candidate,
            "previous": previous,
            "restart": {"before": first_start, "after": second_start},
            "typed_rows_after_restart_and_restore": EXPECTED_ROWS,
            "old_image_untyped_read": [["free-form"]],
            "candidate_read_after_old_image_write": [["old-image-write"]],
            "old_image_refused": refused,
        }
    finally:
        containers = docker("ps", "-aq", "--filter", f"label={label}").splitlines()
        for container in containers:
            (config,) = json.loads(docker("container", "inspect", container))
            if (config["Config"].get("Labels") or {}).get(LABEL_KEY) != run_id:
                raise RuntimeError(f"Unexpected ownership label for container {container}")
            docker("rm", "-f", container)
        volumes = docker("volume", "ls", "-q", "--filter", f"label={label}").splitlines()
        if volume in volumes:
            (config,) = json.loads(docker("volume", "inspect", volume))
            if (config.get("Labels") or {}).get(LABEL_KEY) != run_id:
                raise RuntimeError(f"Unexpected ownership label for volume {volume}")
            docker("volume", "rm", volume)
        docker("volume", "inspect", volume, error_contains=("no such volume",))
        docker("container", "inspect", server, error_contains=("no such container",))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="locally available Linux candidate image")
    parser.add_argument("--old-image", required=True, help="local image from before native typing")
    args = parser.parse_args()
    print(json.dumps(test_images(image_config(args.image), image_config(args.old_image)), indent=2))


if __name__ == "__main__":
    main()
