"""Verify the rendered native TLS Compose profile without starting containers."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
SECRETS = (
    ("BRAINWIRE_AUTH_POLICY_FILE", "BRAINWIRE_SERVER_AUTH_FILE",
     "brainwire-auth-policy", "brainwire-policy.json"),
    ("BRAINWIRE_HEALTH_AUTH_FILE", "BRAINWIRE_BOLT_AUTH_FILE",
     "brainwire-health-auth", "brainwire-health-auth.json"),
    ("BRAINWIRE_NATIVE_TLS_CHAIN_FILE", "BRAINWIRE_SERVER_TLS_CERT_FILE",
     "brainwire-native-tls-chain", "brainwire-native-tls-chain.pem"),
    ("BRAINWIRE_NATIVE_TLS_KEY_FILE", "BRAINWIRE_SERVER_TLS_KEY_FILE",
     "brainwire-native-tls-key", "brainwire-native-tls-key.pem"),
    ("BRAINWIRE_NATIVE_TLS_CA_FILE", "BRAINWIRE_BOLT_TLS_CA_FILE",
     "brainwire-native-tls-ca", "brainwire-native-tls-ca.pem"),
)


def run(compose):
    command = [*compose, "--env-file", os.devnull, "-p", "brainwire-native-tls-check"]
    for filename in ("compose.yaml", "compose.auth.yaml", "compose.native-tls.yaml"):
        command.extend(("-f", str(ROOT / "deploy" / filename)))
    command.extend(("config", "--format", "json"))
    env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    env["BRAINWIRE_NATIVE_TLS_IMAGE"] = "brainwire-native-tls:test"

    def render(values):
        return subprocess.run(command, cwd=ROOT, env=values, capture_output=True,
                              text=True, timeout=30)

    with tempfile.TemporaryDirectory(prefix="brainwire-native-tls-render-") as temporary:
        for variable, _, _, filename in SECRETS:
            file = Path(temporary) / filename
            file.touch(mode=0o600)
            env[variable] = str(file)
        result = render(env)
        if result.returncode:
            raise RuntimeError(result.stderr)
        config = json.loads(result.stdout)
        assert set(config["services"]) == {"brainwire"}
        service = config["services"]["brainwire"]
        assert service["image"] == env["BRAINWIRE_NATIVE_TLS_IMAGE"]
        rejected = render({key: value for key, value in env.items() if key != "BRAINWIRE_NATIVE_TLS_IMAGE"})
        assert rejected.returncode and "BRAINWIRE_NATIVE_TLS_IMAGE" in rejected.stderr
        assert service.get("command") in (None, ["serve"])
        ports = service["ports"]
        assert len(ports) == 1 and ports[0]["host_ip"] == "127.0.0.1"
        assert ports[0]["target"] == 7687
        assert service["healthcheck"]["test"] == [
            "CMD", "brainwire", "probe", "--mode", "ready", "--timeout-ms", "500",
        ]
        assert service["environment"]["BRAINWIRE_SERVER_DATA_DIR"] == "/data"
        assert service["environment"]["BRAINWIRE_SERVER_LISTEN"] == "0.0.0.0:7687"
        assert [(item["type"], item["source"], item["target"]) for item in service["volumes"]] == [
            ("volume", "brainwire-data", "/data"),
        ]
        assert "BRAINWIRE_ALLOW_PLAINTEXT_BOLT_AUTH" not in service["environment"]
        assert {item["source"]: item["target"] for item in service["secrets"]} == {
            name: filename for _, _, name, filename in SECRETS
        }
        for variable, server_variable, name, filename in SECRETS:
            assert service["environment"][server_variable] == f"/run/secrets/{filename}"
            assert Path(config["secrets"][name]["file"]).samefile(env[variable])
            rejected = render({key: value for key, value in env.items() if key != variable})
            assert rejected.returncode and variable in rejected.stderr, rejected.stderr
    print("PASS: protected native TLS Compose secrets, ready probe, loopback and data volume")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compose-bin", help="standalone Compose executable for config rendering")
    args = parser.parse_args()
    run([args.compose_bin] if args.compose_bin else ["docker", "compose"])
