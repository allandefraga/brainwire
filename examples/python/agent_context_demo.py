"""Read the README demo through the three agent-context helpers.

After writing the README's Customer/Segment example, run from the checkout:
    .venv-brainwire/bin/python -m examples.python.agent_context_demo

This example performs reads only. Its local provider returns a fixed demo ID;
it is wiring for a configured provider, not text search, an index or a model.
For a protected server, set BRAINWIRE_BOLT_AUTH_FILE as in the Python guide.
"""
import argparse
import json

from brainwire import connect, schema_paths, search_context, trace_paths
from examples.python.connection import auth_from_env


def read_demo(graph):
    """Inspect existing demo data; never create or overwrite user records."""
    def fixed_candidate(query, *, graph_name, label, limit, timeout):
        # A real application replaces this with its own scoped search provider.
        return ["customer-1"] if graph_name == graph.database and label == "Customer" else []

    schema = schema_paths(graph, "Customer", "Segment", max_hops=1, max_paths=1)
    candidates = search_context(
        graph, "Customer", "Ada", providers={"fixed_demo_id": fixed_candidate},
        properties=["name"], limit=1,
    )
    connections = trace_paths(
        graph, ("Customer", "customer-1"), ("Segment", "segment-1"),
        relationship="IN_SEGMENT", max_hops=1, limit=1,
    )
    ready = bool(candidates["records"] and connections["paths"])
    return {
        "status": "ok" if ready else "demo_data_unavailable",
        "message": ("Read declared routes, a visible candidate and a stored connection."
                    if ready else
                    "Run the README quickstart write example first. The demo records or "
                    "connection are missing or unavailable to this identity."),
        "provider": "Fixed demo ID; ignores query text and performs no search.",
        "schema_routes": schema,
        "candidate_context": candidates,
        "stored_connections": connections,
        "snapshot_note": "These three reads do not share a transaction snapshot.",
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--database", default="demo")
    args = parser.parse_args(argv)
    with connect(args.uri, args.database, auth=auth_from_env(), connection_timeout=5) as graph:
        result = read_demo(graph)
    print(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False))
    return 0 if result["status"] == "ok" else 1


if __name__ == "__main__":
    raise SystemExit(main())
