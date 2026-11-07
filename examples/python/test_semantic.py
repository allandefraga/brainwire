import copy
import json
import itertools
import unittest

from brainwire.context import ContextLimitError
from brainwire.semantic import schema_paths


def node(label, **extra):
    return {"label": label, "properties": ["entity_id", "title"], **extra}


def edge(name, source, target, **extra):
    return {"name": name, "source": source, "target": target,
            "properties": ["at"], **extra}


class Graph:
    database = "workspace"

    def __init__(self, nodes, relationships):
        self.value = {"version": 7, "nodes": nodes, "relationships": relationships,
                      "indexesReady": True}
        self.calls = 0

    def schema(self):
        self.calls += 1
        return self.value

    def stream_cypher(self, *args, **kwargs):
        raise AssertionError("schema paths must never query records")


class SchemaPathsTests(unittest.TestCase):
    def test_direct_paths_preserve_meaning_and_read_schema_once(self):
        graph = Graph([node("Ticket", description="An issue"), node("Project")],
                      [edge("BLOCKS", "Ticket", "Project", description="Prevents delivery",
                            mergeKeyFields=["at"])])
        result = schema_paths(graph, "Ticket", "Project")
        self.assertEqual(graph.calls, 1)
        self.assertTrue(result["schema_only"])
        self.assertEqual(result["graph"], "workspace")
        self.assertEqual(result["catalog_version"], 7)
        self.assertFalse(result["truncated"])
        self.assertEqual(result["paths"][0]["nodes"][0]["description"], "An issue")
        self.assertEqual(result["paths"][0]["relationships"][0]["name"], "BLOCKS")
        self.assertEqual(result["paths"][0]["relationships"][0]["mergeKeyFields"], ["at"])
        self.assertEqual(result["paths"][0]["hops"], 1)
        self.assertNotIn("entity_id", result["paths"][0]["nodes"][0])

    def test_breadth_first_order_is_deterministic_and_preserves_parallel_types(self):
        relationships = [edge("Z_DIRECT", "A", "D"), edge("B_TO_D", "B", "D"),
                         edge("A_TO_B", "A", "B"), edge("A_DIRECT", "A", "D"),
                         edge("C_TO_D", "C", "D"), edge("A_TO_C", "A", "C")]
        graph = Graph([node(label) for label in "ABCD"], relationships)
        result = schema_paths(graph, "A", "D")
        names = [[item["name"] for item in path["relationships"]] for path in result["paths"]]
        self.assertEqual(names, [["A_DIRECT"], ["Z_DIRECT"], ["A_TO_B", "B_TO_D"],
                                 ["A_TO_C", "C_TO_D"]])
        graph.value["relationships"].reverse()
        graph.value["nodes"].reverse()
        self.assertEqual(schema_paths(graph, "A", "D"), result)

    def test_repeated_labels_and_self_loops_are_bounded_by_hops(self):
        graph = Graph([node("Person"), node("Project")],
                      [edge("KNOWS", "Person", "Person"), edge("OWNS", "Person", "Project")])
        result = schema_paths(graph, "Person", "Project", max_hops=3)
        self.assertEqual([[relation["name"] for relation in path["relationships"]]
                          for path in result["paths"]],
                         [["OWNS"], ["KNOWS", "OWNS"], ["KNOWS", "KNOWS", "OWNS"]])
        same = schema_paths(graph, "Person", "Person")
        self.assertEqual(same["paths"][0]["hops"], 0)
        self.assertEqual(same["paths"][0]["relationships"], [])

    def test_no_route_unknown_labels_and_direction_return_empty_paths(self):
        graph = Graph([node("A"), node("B")], [edge("CONNECTS", "A", "B")])
        for source, target in [("B", "A"), ("Missing", "B"), ("A", "Missing")]:
            self.assertEqual(schema_paths(graph, source, target)["paths"], [])

    def test_hop_limit_and_path_limit_are_explicit(self):
        graph = Graph([node(label) for label in "ABC"],
                      [edge("FIRST", "A", "B"), edge("SECOND", "B", "C"),
                       edge("DIRECT", "A", "C")])
        first = schema_paths(graph, "A", "C", max_paths=1)
        self.assertTrue(first["truncated"])
        self.assertEqual(len(first["paths"]), 1)
        only_direct = schema_paths(graph, "A", "C", max_hops=1, max_paths=1)
        self.assertFalse(only_direct["truncated"])
        self.assertEqual(only_direct["paths"][0]["hops"], 1)

    def test_small_cyclic_catalogs_match_exhaustive_edge_sequence_oracle(self):
        # Enumerate edge sequences independently of the implementation's queue.
        # Include loops, a two-way cycle, parallel routes and edges out of C:
        # reaching the target must stop a route even if another route returns.
        declarations = [edge("AA", "A", "A"), edge("AB", "A", "B"),
                        edge("AC", "A", "C"), edge("BA", "B", "A"),
                        edge("BC", "B", "C"), edge("CA", "C", "A"),
                        edge("CC", "C", "C")]
        for mask in (0, 1, 7, 14, 27, 42, 63, 85, 108, 127):
            selected = [relation for bit, relation in enumerate(declarations)
                        if mask & (1 << bit)]
            graph = Graph([node(label) for label in "ABC"], list(reversed(selected)))
            for source, target in (("A", "C"), ("B", "A"), ("C", "B"), ("A", "A")):
                for max_hops in (1, 2, 3, 4):
                    if source == target:
                        expected = [()]
                    else:
                        expected = []
                        for length in range(1, max_hops + 1):
                            for sequence in itertools.product(selected, repeat=length):
                                if sequence[0]["source"] != source or sequence[-1]["target"] != target:
                                    continue
                                if any(left["target"] != right["source"]
                                       for left, right in zip(sequence, sequence[1:])):
                                    continue
                                if any(relation["target"] == target for relation in sequence[:-1]):
                                    continue
                                expected.append(tuple((relation["name"], relation["target"])
                                                      for relation in sequence))
                        expected.sort(key=lambda route: (len(route), route))
                    for max_paths in (1, 2, 20):
                        with self.subTest(mask=mask, source=source, target=target,
                                          max_hops=max_hops, max_paths=max_paths):
                            result = schema_paths(graph, source, target,
                                                  max_hops=max_hops, max_paths=max_paths)
                            actual = [tuple((relation["name"], relation["target"])
                                            for relation in path["relationships"])
                                      for path in result["paths"]]
                            self.assertEqual(actual, expected[:max_paths])
                            self.assertEqual(result["truncated"], len(expected) > max_paths)
                            for path in result["paths"]:
                                self.assertEqual(path["hops"], len(path["relationships"]))
                                self.assertEqual([item["label"] for item in path["nodes"]],
                                                 [source] + [relation["target"]
                                                             for relation in path["relationships"]])

    def test_invalid_options_fail_before_schema_read(self):
        graph = Graph([], [])
        for options in [{"max_hops": 0}, {"max_hops": 5}, {"max_hops": True},
                        {"max_paths": 0}, {"max_paths": 21}, {"max_paths": 1.0},
                        {"max_bytes": 0}, {"max_bytes": (1 << 20) + 1}]:
            with self.subTest(options=options), self.assertRaises(ValueError):
                schema_paths(graph, "A", "B", **options)
        with self.assertRaises(ValueError):
            schema_paths(graph, "A) RETURN 1", "B")
        self.assertEqual(graph.calls, 0)

    def test_malformed_schema_is_rejected_without_returning_partial_paths(self):
        valid = Graph([node("A"), node("B")], [edge("LINK", "A", "B")]).value
        bad = [None, {}, {**valid, "version": True}, {**valid, "nodes": {}},
               {**valid, "nodes": [node("A"), node("A")]},
               {**valid, "relationships": [edge("LINK", "A", "Missing")]},
               {**valid, "relationships": [edge("LINK", "A", "B", properties="value")]},
               {**valid, "relationships": [edge("LINK", "A", "B")] * 2},
               {**valid, "nodes": [node("A", description=123), node("B")]},
               {**valid, "relationships": [edge("LINK", "A", "B", mergeKeyFields=["missing"])]}]
        for value in bad:
            graph = Graph([], [])
            graph.value = value
            with self.subTest(value=value), self.assertRaises(ValueError):
                schema_paths(graph, "A", "B")
            self.assertEqual(graph.calls, 1)

    def test_output_byte_limit_counts_utf8_metadata_and_envelope(self):
        graph = Graph([node("A", description="é" * 50), node("B")], [edge("LINK", "A", "B")])
        full = schema_paths(graph, "A", "B")
        size = len(json.dumps(full, ensure_ascii=False, allow_nan=False).encode("utf-8"))
        self.assertEqual(schema_paths(graph, "A", "B", max_bytes=size), full)
        with self.assertRaises(ContextLimitError):
            schema_paths(graph, "A", "B", max_bytes=size - 1)
        with self.assertRaises(ContextLimitError):
            schema_paths(Graph([], []), "A", "B", max_bytes=1)

    def test_expansion_bound_refuses_explosive_schema(self):
        graph = Graph([node("A"), node("Target")],
                      [edge(f"LOOP_{i:02}", "A", "A") for i in range(30)])
        with self.assertRaisesRegex(ContextLimitError, "expansion"):
            schema_paths(graph, "A", "Target", max_hops=4)

    def test_parallel_signatures_keep_consistent_type_metadata_and_errors_propagate(self):
        graph = Graph([node(label) for label in "ABC"],
                      [edge("LINK", "A", "B"), edge("LINK", "B", "C")])
        self.assertEqual(schema_paths(graph, "A", "C")["paths"][0]["hops"], 2)
        graph.value["relationships"][1]["description"] = "Different definition"
        with self.assertRaisesRegex(ValueError, "consistent"):
            schema_paths(graph, "A", "C")
        class Denied(Graph):
            def schema(self):
                raise PermissionError("graph read denied")
        with self.assertRaises(PermissionError):
            schema_paths(Denied([], []), "A", "B")

    def test_collection_and_total_string_limits_fail_before_path_search(self):
        graph = Graph([node("A", properties=["value"] * 65537)], [])
        with self.assertRaisesRegex(ContextLimitError, "collection"):
            schema_paths(graph, "A", "A")
        graph = Graph([node(f"Label{i}", description="x" * 4096) for i in range(257)], [])
        with self.assertRaisesRegex(ContextLimitError, "string"):
            schema_paths(graph, "Label0", "Label1")
        graph = Graph([node("A")], [edge("LOOP", "A", "A")] * 4097)
        with self.assertRaisesRegex(ContextLimitError, "signature"):
            schema_paths(graph, "A", "A")

    def test_schema_limits_are_enforced_and_input_is_not_mutated(self):
        graph = Graph([node("A"), node("B")], [edge("LINK", "A", "B")])
        before = copy.deepcopy(graph.value)
        schema_paths(graph, "A", "B")
        self.assertEqual(graph.value, before)
        graph.value["nodes"] = [node("A")] * 4097
        with self.assertRaises(ContextLimitError):
            schema_paths(graph, "A", "B")
        graph = Graph([node("A", description="x" * 4097), node("B")], [])
        with self.assertRaises(ValueError):
            schema_paths(graph, "A", "B")


if __name__ == "__main__":
    unittest.main()
