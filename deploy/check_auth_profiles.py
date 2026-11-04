"""Render and check optional auth overlays locally; never contact a cluster or deploy.

Requires Docker Compose, Docker's stack config command and kubectl/kustomize.
Uses only Python's standard library. Run: python3 deploy/check_auth_profiles.py
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import re

ROOT = Path(__file__).resolve().parents[1]
POLICY = "/run/secrets/brainwire-policy.json"
HEALTH = "/run/secrets/brainwire-health-auth.json"
READY = ["CMD", "brainwire", "probe", "--mode", "ready", "--timeout-ms", "500"]


def run(args, env):
    result = subprocess.run(args, cwd=ROOT, env=env, capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{' '.join(args)} failed: {result.stderr}")
    return result.stdout


def check():
    env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    compose = ["docker", "compose", "--env-file", os.devnull]
    checked = []
    with tempfile.TemporaryDirectory(prefix="brainwire-auth-render-") as temporary:
        policy = Path(temporary) / "policy.json"
        health = Path(temporary) / "health.json"
        # Rendering checks mounts, not policy contents; these hold no credentials.
        policy.write_text("{}\n")
        health.write_text("{}\n")
        env.update(BRAINWIRE_AUTH_POLICY_FILE=str(policy), BRAINWIRE_HEALTH_AUTH_FILE=str(health),
                   BRAINWIRE_SWARM_NODE_ID="synthetic-node-id")
        for profile in ("compose", "swarm"):
            args = compose + ["-f", f"deploy/{profile}.yaml", "-f", f"deploy/{profile}.auth.yaml"]
            rendered = json.loads(run(args + ["config", "--format", "json"], env))
            service = rendered["services"]["brainwire"]
            assert service["environment"]["BRAINWIRE_SERVER_AUTH_FILE"] == POLICY
            assert service["environment"]["BRAINWIRE_BOLT_AUTH_FILE"] == HEALTH
            assert service["healthcheck"]["test"] == READY
            assert {item["target"] for item in service["secrets"]} == {
                "brainwire-policy.json", "brainwire-health-auth.json"}
            if profile == "compose":
                assert service["ports"][0]["host_ip"] == "127.0.0.1"
                assert rendered["secrets"]["brainwire-auth-policy"]["file"] == str(policy)
                assert rendered["secrets"]["brainwire-health-auth"]["file"] == str(health)
                for missing in ("BRAINWIRE_AUTH_POLICY_FILE", "BRAINWIRE_HEALTH_AUTH_FILE"):
                    without = dict(env)
                    del without[missing]
                    rejected = subprocess.run(args + ["config"], cwd=ROOT, env=without,
                                              capture_output=True, text=True, timeout=30)
                    assert rejected.returncode != 0 and missing in rejected.stderr
                checked.append("Compose requires both secret paths and retains the loopback binding")
            else:
                run(["docker", "stack", "config", "-c", "deploy/swarm.yaml", "-c",
                     "deploy/swarm.auth.yaml"], env)
                assert service["deploy"]["replicas"] == 1
                assert service["deploy"]["update_config"]["order"] == "stop-first"
                assert rendered["volumes"]["brainwire-data"]["external"]
                assert all(item["external"] for item in rendered["secrets"].values())
                for item in service["secrets"]:
                    mode = item["mode"]
                    assert (int(mode, 8) if isinstance(mode, str) else mode) == 0o400
                    assert item["uid"] == item["gid"] == "10001"
                checked.append("Swarm validates external secrets, UID 10001/mode 0400 and retained single replica")
            checked.append(f"{profile}: policy/client mount paths and inherited ready probe")
        base = json.loads(run(compose + ["-f", "deploy/compose.yaml", "config", "--format", "json"], env))
        assert "BRAINWIRE_SERVER_AUTH_FILE" not in base["services"]["brainwire"]["environment"]
        assert "secrets" not in base["services"]["brainwire"]

    # Check kustomize's normalized emitted stanzas without introducing a YAML parser.
    kubernetes = " ".join(run(["kubectl", "kustomize", "deploy/kubernetes-auth"], env).split())
    for stanza in (
        f"name: BRAINWIRE_SERVER_AUTH_FILE value: {POLICY}",
        f"name: BRAINWIRE_BOLT_AUTH_FILE value: {HEALTH}",
        "mountPath: /run/secrets name: auth readOnly: true",
        "name: auth secret: defaultMode: 288 items: - key: policy.json path: brainwire-policy.json "
        "- key: health.json path: brainwire-health-auth.json optional: false secretName: brainwire-auth",
        "fsGroup: 10001", "runAsUser: 10001", "replicas: 1",
        "persistentVolumeClaimRetentionPolicy: whenDeleted: Retain whenScaled: Retain",
    ):
        assert stanza in kubernetes, stanza
    for probe, mode in (("startupProbe", "ready"), ("readinessProbe", "ready"), ("livenessProbe", "live")):
        assert f"{probe}: exec: command: - brainwire - probe - --mode - {mode}" in kubernetes
    base = run(["kubectl", "kustomize", "deploy/kubernetes"], env)
    assert "BRAINWIRE_SERVER_AUTH_FILE" not in base and "BRAINWIRE_BOLT_AUTH_FILE" not in base
    package = re.search(r"(?ms)^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)",
                        (ROOT / "Cargo.toml").read_text())
    assert package, "missing workspace.package section"
    version_match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', package.group(1))
    assert version_match, "missing workspace version"
    version = version_match.group(1)
    auth_release = " ".join(run(["kubectl", "kustomize", "deploy/kubernetes-release"], env).split())
    for stanza in (
        f"image: ghcr.io/allandefraga/brainwire:v{version}",
        f"name: BRAINWIRE_SERVER_AUTH_FILE value: {POLICY}",
        f"name: BRAINWIRE_BOLT_AUTH_FILE value: {HEALTH}",
        "name: ghcr-pull",
        "secretName: brainwire-auth",
    ):
        assert stanza in auth_release, stanza
    public_release = " ".join(run(["kubectl", "kustomize", "deploy/kubernetes-public-release"], env).split())
    for stanza in (
        f"image: ghcr.io/allandefraga/brainwire:v{version}",
        f"name: BRAINWIRE_SERVER_AUTH_FILE value: {POLICY}",
        f"name: BRAINWIRE_BOLT_AUTH_FILE value: {HEALTH}",
        "secretName: brainwire-auth",
    ):
        assert stanza in public_release, stanza
    assert "imagePullSecrets" not in public_release
    protected = " ".join(run(["kubectl", "kustomize", "deploy/kubernetes-disruption-protected"], env).split())
    for stanza in (
        "apiVersion: policy/v1 kind: PodDisruptionBudget metadata: name: brainwire "
        "spec: minAvailable: 1 selector: matchLabels: app: brainwire",
        "replicas: 1",
        "persistentVolumeClaimRetentionPolicy: whenDeleted: Retain whenScaled: Retain",
        f"image: ghcr.io/allandefraga/brainwire:v{version}",
        f"name: BRAINWIRE_SERVER_AUTH_FILE value: {POLICY}",
    ):
        assert stanza in protected, stanza
    assert protected.count("kind: PodDisruptionBudget") == 1
    assert "imagePullSecrets" not in protected
    isolated = " ".join(run(["kubectl", "kustomize", "deploy/kubernetes-network-isolated"], env).split())
    network_policy = (
        "apiVersion: networking.k8s.io/v1 kind: NetworkPolicy metadata: name: brainwire-client-access "
        "spec: ingress: - from: - podSelector: matchLabels: brainwire-client: \"true\" "
        "ports: - port: 7687 protocol: TCP podSelector: matchLabels: app: brainwire policyTypes: - Ingress"
    )
    assert network_policy in isolated, "network policy must only admit labeled same-namespace Bolt clients"
    assert isolated.count("kind: NetworkPolicy") == 1
    assert "namespaceSelector" not in isolated and "ipBlock" not in isolated
    for stanza in (
        "replicas: 1", "kind: PodDisruptionBudget",
        "persistentVolumeClaimRetentionPolicy: whenDeleted: Retain whenScaled: Retain",
        f"image: ghcr.io/allandefraga/brainwire:v{version}",
        f"name: BRAINWIRE_SERVER_AUTH_FILE value: {POLICY}",
    ):
        assert stanza in isolated, stanza
    assert "imagePullSecrets" not in isolated
    single_writer = " ".join(run(["kubectl", "kustomize", "deploy/kubernetes-single-writer"], env).split())
    for stanza in (
        "accessModes: - ReadWriteOncePod", "replicas: 1",
        "persistentVolumeClaimRetentionPolicy: whenDeleted: Retain whenScaled: Retain",
        f"image: ghcr.io/allandefraga/brainwire:v{version}",
        f"name: BRAINWIRE_SERVER_AUTH_FILE value: {POLICY}",
        f"name: BRAINWIRE_BOLT_AUTH_FILE value: {HEALTH}",
        "secretName: brainwire-auth", "readOnlyRootFilesystem: true",
    ):
        assert stanza in single_writer, stanza
    assert single_writer.count("accessModes:") == 1
    assert "accessModes: - ReadWriteOnce " not in single_writer
    assert "imagePullSecrets" not in single_writer
    assert "accessModes: - ReadWriteOnce " in public_release
    checked.extend(["Kubernetes required read-only Secret projection, fsGroup, probes and retained single replica",
                    "Development Compose and Kubernetes base profiles remain without auth mounts",
                    "Versioned private/public Kubernetes overlays combine auth with appropriate GHCR pull behavior",
                    "Optional disruption budget protects the single authenticated persistent instance from voluntary eviction",
                    "Optional ingress policy only admits labeled same-namespace Bolt clients and retains auth, PVC and disruption budget",
                    "Optional single-writer profile requests ReadWriteOncePod for new CSI volumes and retains auth, version and single replica"])
    return {"mode": "local-render-only", "checks": checked}


if __name__ == "__main__":
    print(json.dumps(check(), indent=2))
