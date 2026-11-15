import unittest

from brainwire import require_relationship_keys


def event(source, target, keys=None):
    declaration = {"source": source, "target": target, "name": "EVENT",
                   "properties": ["kind", "event_id"]}
    if keys is not None:
        declaration["mergeKeyFields"] = keys
    return declaration


class Graph:
    def __init__(self, relationships):
        self.calls = 0
        self.catalog = {
            "version": 7, "indexesReady": True,
            "nodes": [{"label": label, "properties": []} for label in ("Person", "Ticket", "Decision")],
            "relationships": relationships,
        }

    def schema(self):
        self.calls += 1
        return self.catalog


class RelationshipGuardTests(unittest.TestCase):
    def test_exact_keys_across_multiple_signatures_return_observed_version(self):
        graph = Graph([event("Person", "Ticket", ["kind", "event_id"]),
                       event("Person", "Decision", ["kind", "event_id"])])
        self.assertEqual(require_relationship_keys(graph, "Person", "EVENT", "Ticket",
                                                   ("kind", "event_id")), 7)
        self.assertEqual(graph.calls, 1)

    def test_missing_unkeyed_or_inexact_declaration_is_refused(self):
        for relationships in ([], [event("Person", "Decision", ["kind"])],
                              [event("Ticket", "Person", ["kind"])], [event("Person", "Ticket")],
                              [event("Person", "Ticket", ["kind", "event_id"])],
                              [event("Person", "Ticket", ["event_id"])]):
            with self.subTest(relationships=relationships):
                graph = Graph(relationships)
                with self.assertRaises(ValueError):
                    require_relationship_keys(graph, "Person", "EVENT", "Ticket", ["kind"])
                self.assertEqual(graph.calls, 1)

    def test_key_order_must_match_every_signature(self):
        for keys in (["kind", "event_id"], ["event_id", "kind"]):
            with self.subTest(keys=keys):
                graph = Graph([event("Person", "Ticket", keys),
                               event("Person", "Decision", ["event_id", "kind"])])
                with self.assertRaisesRegex(ValueError, "expected merge keys in order"):
                    require_relationship_keys(graph, "Person", "EVENT", "Ticket", ["kind", "event_id"])
                self.assertEqual(graph.calls, 1)

    def test_mismatched_sibling_signature_is_refused(self):
        for keys in (None, ["event_id"], ["kind", "kind"], ["unknown"]):
            with self.subTest(keys=keys):
                graph = Graph([event("Person", "Ticket", ["kind"]), event("Decision", "Ticket", keys)])
                with self.assertRaises(ValueError):
                    require_relationship_keys(graph, "Person", "EVENT", "Ticket", ["kind"])
                self.assertEqual(graph.calls, 1)

    def test_invalid_arguments_are_refused_before_reading_the_catalog(self):
        for arguments in (("", "EVENT", "Ticket", ["kind"]),
                          ("Person", "EVENT) DELETE n", "Ticket", ["kind"]),
                          ("Person", "EVENT", None, ["kind"]),
                          ("Person", "EVENT", "Ticket", []),
                          ("Person", "EVENT", "Ticket", "kind"),
                          ("Person", "EVENT", "Ticket", [1]),
                          ("Person", "EVENT", "Ticket", ["acl_allow"]),
                          ("Person", "EVENT", "Ticket", ["kind", "kind"])):
            with self.subTest(arguments=arguments):
                graph = Graph([])
                with self.assertRaises(ValueError):
                    require_relationship_keys(graph, *arguments)
                self.assertEqual(graph.calls, 0)


if __name__ == "__main__":
    unittest.main()
