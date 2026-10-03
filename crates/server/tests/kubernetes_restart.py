"""Local kind acceptance for the actual retained-PVC StatefulSet.

Requires Docker, kubectl, kind and the Neo4j Python driver. Uses a unique cluster
and a private kubeconfig; removes only that cluster, even after a failed check.
Build --image locally first. This tests one node, not replication or node loss.
"""
import argparse
from contextlib import contextmanager, suppress
import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import uuid

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired


def run(kind_binary, image, rows, node_image):
    root = Path(__file__).resolve().parents[3]
    name = f"brainwire-k8s-{uuid.uuid4().hex[:12]}"
    namespace = "brainwire-test"
    with tempfile.TemporaryDirectory(prefix="brainwire-k8s-") as temporary:
        temporary = Path(temporary)
        kubeconfig = temporary / "kubeconfig"
        env = dict(os.environ, KUBECONFIG=str(kubeconfig), KIND_EXPERIMENTAL_PROVIDER="docker")

        def command(args, text=None, timeout=180):
            try:
                result = subprocess.run(args, input=text, text=True, capture_output=True, env=env, timeout=timeout)
            except subprocess.TimeoutExpired as error:
                def decoded(value):
                    return value.decode(errors="replace") if isinstance(value, bytes) else value or ""
                raise RuntimeError(f"{' '.join(args[:5])} timed out after {timeout}s:\n"
                                   + decoded(error.stdout)[-6000:] + decoded(error.stderr)[-6000:]) from error
            if result.returncode:
                raise RuntimeError(f"{' '.join(args[:5])}: {result.stdout}\n{result.stderr}")
            return result.stdout

        kubectl_args = ["kubectl", "--kubeconfig", str(kubeconfig), "--context", f"kind-{name}", "--namespace", namespace]
        def kube(*args, text=None, timeout=180):
            return command(kubectl_args + list(args), text, timeout)
        def ready():
            deadline = time.monotonic() + 180
            detail = "no Pod response"
            while time.monotonic() < deadline:
                try:
                    result = subprocess.run(kubectl_args + ["get", "pod/brainwire-0", "-o", "json"],
                                            env=env, capture_output=True, text=True, timeout=10)
                except subprocess.TimeoutExpired:
                    continue
                detail = result.stdout + result.stderr
                if result.returncode == 0:
                    pod = json.loads(result.stdout)
                    if any(c.get("type") == "Ready" and c.get("status") == "True"
                           for c in pod.get("status", {}).get("conditions", [])):
                        return
                time.sleep(1)
            raise RuntimeError("Pod did not become ready: " + detail)
        def stop():
            kube("scale", "statefulset/brainwire", "--replicas=0")
            kube("wait", "--for=delete", "pod/brainwire-0", "--timeout=90s")
        def start():
            kube("scale", "statefulset/brainwire", "--replicas=1")
            ready()
        def offline_job(label, args):
            spec = copy.deepcopy(json.loads(kube("get", "statefulset/brainwire", "-o", "json"))["spec"]["template"]["spec"])
            spec["restartPolicy"] = "Never"
            spec["volumes"] = [{"name": "data", "persistentVolumeClaim": {"claimName": "data-brainwire-0"}}]
            container = spec["containers"][0]
            container["args"] = args
            for probe in ("startupProbe", "readinessProbe", "livenessProbe"):
                container.pop(probe, None)
            job = {"apiVersion": "batch/v1", "kind": "Job", "metadata": {"name": label}, "spec": {"backoffLimit": 0, "template": {"metadata": {"labels": {"app": "brainwire-admin"}}, "spec": spec}}}
            kube("apply", "-f", "-", text=json.dumps(job))
            try:
                kube("wait", "--for=condition=complete", f"job/{label}", "--timeout=90s")
                return kube("logs", f"job/{label}")
            except Exception:
                print(kube("logs", f"job/{label}"), flush=True)
                raise
            finally:
                kube("delete", f"job/{label}", "--wait=true")

        @contextmanager
        def endpoint():
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            with (temporary / "port-forward.log").open("w") as log:
                process = subprocess.Popen(kubectl_args + ["port-forward", "--address", "127.0.0.1", "pod/brainwire-0", f"{port}:7687"], env=env, stdout=log, stderr=log)
                try:
                    for _ in range(100):
                        if process.poll() is not None:
                            raise RuntimeError((temporary / "port-forward.log").read_text())
                        try:
                            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                                break
                        except OSError:
                            time.sleep(0.1)
                    else:
                        raise RuntimeError("port-forward never became ready")
                    yield f"bolt://127.0.0.1:{port}"
                finally:
                    process.terminate()
                    with suppress(subprocess.TimeoutExpired):
                        process.wait(timeout=5)
                    if process.poll() is None:
                        process.kill()
                        process.wait()

        def verify(uri, database="client_memory"):
            with GraphDatabase.driver(uri, auth=None, connection_timeout=3) as driver:
                with driver.session(database=database) as session:
                    assert session.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"] == rows
                    assert session.run("MATCH (c:Customer) WHERE c.email='k8s-0@example.test' RETURN c.entity_id AS id").single()["id"] == "k8s-0"
                    assert session.run("MATCH (c:Customer) WHERE c.email='pending-k8s' RETURN count(*) AS n").single()["n"] == 0
                    assert session.run("MATCH (c:Customer) WHERE c.segment=0.0 RETURN count(*) AS n").single()["n"] == (rows + 6) // 7
                    assert session.run("MATCH (c:Customer) WHERE c.segment=999999 RETURN count(*) AS n").single()["n"] == 0

        try:
            print(f"creating isolated kind cluster {name}", flush=True)
            command([kind_binary, "create", "cluster", "--name", name, "--kubeconfig", str(kubeconfig), "--image", node_image, "--wait", "90s"], timeout=600)
            print("cluster ready; waiting for local image", flush=True)
            # Allows a separately running local Docker build to finish.
            deadline = time.monotonic() + 900
            while time.monotonic() < deadline:
                try:
                    available = subprocess.run(["docker", "image", "inspect", image],
                                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
                except subprocess.TimeoutExpired:
                    continue
                if available.returncode == 0:
                    break
                time.sleep(5)
            else:
                raise RuntimeError("local image was not available after 15 minutes")
            command([kind_binary, "load", "docker-image", image, "--name", name], timeout=180)
            print("loaded local image; applying retained-PVC StatefulSet", flush=True)
            kube("create", "namespace", namespace)
            manifest = command(["kubectl", "kustomize", str(root / "deploy/kubernetes")])
            assert manifest.count("image: brainwire:local") == 1
            kube("apply", "-f", "-", text=manifest.replace("image: brainwire:local", f"image: {image}"))
            ready()
            version = json.loads(kube("version", "-o", "json"))["serverVersion"]["gitVersion"]
            pvc_uid = kube("get", "pvc/data-brainwire-0", "-o", "jsonpath={.metadata.uid}")
            stop()
            offline_job("initialize", ["db", "apply-ontology", "client_memory", "/opt/brainwire/example-schema.json"])
            start()
            with endpoint() as uri:
                with GraphDatabase.driver(uri, auth=None) as driver:
                    with driver.session(database="client_memory") as session:
                        for offset in range(0, rows, 250):
                            batch = [{"id": f"k8s-{i}", "email": f"k8s-{i}@example.test", "segment": i % 7} for i in range(offset, min(offset + 250, rows))]
                            session.run("UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email, c.segment=row.segment", rows=batch).consume()
                verify(uri)
            recoveries = []
            for action in ("container-stop-zero-timeout", "pod-recreation"):
                pod = json.loads(kube("get", "pod/brainwire-0", "-o", "json"))
                old_uid = pod["metadata"]["uid"]
                old_restarts = pod["status"]["containerStatuses"][0]["restartCount"]
                with endpoint() as uri:
                    with GraphDatabase.driver(uri, auth=None) as driver:
                        with driver.session(database="client_memory") as session:
                            tx = session.begin_transaction()
                            tx.run("MATCH (c:Customer {entity_id:'k8s-0'}) SET c.email='pending-k8s', c.segment=999999").consume()
                            assert tx.run("MATCH (c:Customer) WHERE c.email='pending-k8s' RETURN count(*) AS n").single()["n"] == 1
                            try:
                                if action == "container-stop-zero-timeout":
                                    ids = command(["docker", "exec", f"{name}-control-plane", "crictl", "ps", "--label", "io.kubernetes.pod.name=brainwire-0", "--label", f"io.kubernetes.pod.namespace={namespace}", "-q"]).split()
                                    assert len(ids) == 1, ids
                                    command(["docker", "exec", f"{name}-control-plane", "crictl", "stop", "--timeout=0", ids[0]])
                                    deadline = time.monotonic() + 180
                                    while time.monotonic() < deadline:
                                        current = json.loads(kube("get", "pod/brainwire-0", "-o", "json", timeout=10))
                                        statuses = current.get("status", {}).get("containerStatuses", [])
                                        if statuses and statuses[0]["restartCount"] > old_restarts:
                                            break
                                        time.sleep(1)
                                    else:
                                        raise RuntimeError("container did not restart")
                                else:
                                    kube("delete", "pod/brainwire-0", "--grace-period=5", "--wait=true")
                            finally:
                                with suppress(Neo4jError, ServiceUnavailable, SessionExpired):
                                    tx.close()
                ready()
                pod = json.loads(kube("get", "pod/brainwire-0", "-o", "json"))
                if action == "pod-recreation":
                    assert pod["metadata"]["uid"] != old_uid
                else:
                    assert pod["metadata"]["uid"] == old_uid
                assert kube("get", "pvc/data-brainwire-0", "-o", "jsonpath={.metadata.uid}") == pvc_uid
                with endpoint() as uri:
                    verify(uri)
                recoveries.append({"action": action, "podUid": pod["metadata"]["uid"], "restartCount": pod["status"]["containerStatuses"][0]["restartCount"]})
                print(f"verified {action}, same retained PVC", flush=True)
            stop()
            audit = json.loads(offline_job("audit", ["db", "check", "client_memory", "--json"]))
            assert audit["status"] == "ok"
            checkpoint = json.loads(offline_job("checkpoint", ["db", "checkpoint", "client_memory", "--destination", "/data/checkpoint-k8s"]))
            assert checkpoint["status"] == "checkpoint_created"
            checkpoint_audit = json.loads(offline_job("checkpoint-audit", ["db", "check", "checkpoint-k8s", "--json"]))
            assert checkpoint_audit["status"] == "ok"
            start()
            with endpoint() as uri:
                verify(uri, "checkpoint-k8s")
            print(json.dumps({"result": "PASS", "image": image, "kubernetes": version, "rows": rows, "cluster": name, "pvcUid": pvc_uid, "recoveries": recoveries, "audit": audit, "checkpointAudit": checkpoint_audit, "scope": "single kind node, retained local PVC; not replication or node loss"}, indent=2), flush=True)
        finally:
            result = subprocess.run([kind_binary, "delete", "cluster", "--name", name, "--kubeconfig", str(kubeconfig)], env=env, capture_output=True, text=True, timeout=180)
            if result.returncode:
                raise RuntimeError(f"could not clean owned cluster {name}: {result.stderr}")
            print(f"removed isolated cluster {name}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", default="kind")
    parser.add_argument("--image", default="brainwire:local")
    parser.add_argument("--rows", type=int, default=1000)
    parser.add_argument("--node-image", default="kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed")
    args = parser.parse_args()
    if not 1 <= args.rows <= 100000:
        parser.error("rows must be between 1 and 100000")
    run(args.kind, args.image, args.rows, args.node_image)
