"""Seed and query a disposable synthetic company context graph.

Run only against a fresh graph created with this example's ontology. The fixed
IDs are overwritten on replay. The provider below is a fixture, not search.
"""
import argparse
import json

from brainwire import connect, search_context, trace_paths
from examples.python.connection import auth_from_env


def seed(graph):
    graph.upsert_node("Team", "team-platform", {"name": "Platform"})
    graph.upsert_node("Project", "project-release", {"name": "Release"})
    graph.upsert_node("Decision", "decision-retry", {
        "title": "Retry failed requests", "body": "Use exponential backoff.",
        "source_url": "https://example.com/meeting/release"})
    graph.upsert_node("Decision", "decision-other", {
        "title": "Archive old reports", "body": "Keep reports for one year.",
        "source_url": "https://example.com/meeting/reports"})
    graph.upsert_node("Source", "meeting-release", {
        "title": "Release meeting", "url": "https://example.com/meeting/release"})
    graph.upsert_edge(("Team", "team-platform"), "OWNS", ("Project", "project-release"))
    graph.upsert_edge(("Project", "project-release"), "HAS_DECISION", ("Decision", "decision-retry"))
    graph.upsert_edge(("Decision", "decision-retry"), "SUPPORTED_BY", ("Source", "meeting-release"))


def read_context(graph):
    # The bounded scope comes from a separate authorized graph query.
    scope = graph.run_cypher(
        "MATCH (p:Project {entity_id: $project})-[:HAS_DECISION]->(d:Decision) "
        "RETURN d.entity_id AS id LIMIT 100", {"project": "project-release"})
    def sample_provider(query, *, graph_name, label, limit, timeout):
        return ["decision-other", "decision-retry"]
    result = search_context(
        graph, "Decision", "release retries", providers={"fixture": sample_provider},
        candidate_ids=[row["id"] for row in scope], properties=["title", "body"],
        source_fields={"url": "source_url"}, limit=10,
    )
    links = trace_paths(graph, ("Decision", "decision-retry"), ("Source", "meeting-release"),
                        relationship="SUPPORTED_BY", max_hops=1, limit=1)
    return {"context": result, "source_links": links, "scope_size": len(scope),
            "scope_may_be_truncated": len(scope) == 100,
            "note": "Fixture provider, not text or vector search. Scope is unordered; reads use separate snapshots."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--database", default="company_demo")
    parser.add_argument("--seed", action="store_true", help="upsert synthetic records into a disposable graph")
    args = parser.parse_args()
    with connect(args.uri, args.database, auth=auth_from_env(), connection_timeout=5) as graph:
        if args.seed:
            seed(graph)
        print(json.dumps(read_context(graph), indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
