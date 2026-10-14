import unittest
from examples.python.client import GraphClient, request_key


class Result:
    def __init__(self, count):
        self.count = count

    def single(self):
        return {"processed": self.count}


class Driver:
    def __init__(self, counts=()):
        self.committed, self.counts = [], iter(counts)

    def session(self, *, database):
        self.database = database
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def execute_write(self, callback):
        captured = []
        driver = self

        class Tx:
            def run(self, query, **params):
                captured.append((query, params))
                return Result(next(driver.counts, len(params.get("rows", [1]))))

        result = callback(Tx())
        self.committed.append(captured)
        return result


class ClientTests(unittest.TestCase):
    def test_mixed_shapes_preserve_duplicate_order_and_bounded_batches(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=3)
        rows = [{"entity_id":"same", "status":"first"},
                {"entity_id":"same", "status":"second", "tags":["x"]},
                {"entity_id":"same", "status":"last"},
                {"entity_id":"other", "status":None}]
        self.assertEqual(graph.load_nodes("Ticket", iter(rows)), 4)
        self.assertEqual([len(batch) for batch in driver.committed], [3, 1])
        records = [row for batch in driver.committed for _, params in batch for row in params["rows"]]
        self.assertEqual([row["status"] for row in records], ["first","second","last",None])
        self.assertEqual(driver.database,"tickets")

    def test_patch_separates_unchanged_set_and_remove_and_missing_node(self):
        driver = Driver([1,0])
        graph = GraphClient(driver,"tickets")
        self.assertTrue(graph.update_node("Ticket","t-1",{"status":"open"},remove=["priority"]))
        query, params = driver.committed[0][0]
        self.assertNotIn("MERGE",query)
        self.assertEqual(params,{"id":"t-1","property_0":None,"property_1":"open"})
        self.assertFalse(graph.update_node("Ticket","absent",{"status":"closed"}))

    def test_edge_patch_checks_existence_and_null_removal_in_one_transaction(self):
        driver = Driver([1,1,0])
        graph = GraphClient(driver,"pages")
        self.assertTrue(graph.update_edge(("Page","a"),"LINK",("Page","b"),{"weight":2},remove=["old"]))
        self.assertEqual(len(driver.committed[0]),2)
        self.assertEqual(driver.committed[0][1][1],{"source":"a","target":"b","property_0":None,"property_1":2})
        self.assertFalse(graph.update_edge(("Page","a"),"LINK",("Page","missing"),{"weight":3}))
        self.assertEqual(len(driver.committed[1]),1)
        driver = Driver([0])
        with self.assertRaises(ValueError):
            GraphClient(driver,"pages").upsert_edge(("Page","a"),"LINK",("Page","missing"))
        self.assertEqual(driver.committed,[])

    def test_rejects_injection_reserved_fields_invalid_values_and_oversized_rows(self):
        graph = GraphClient(Driver(),"tickets",batch_bytes=100)
        for label, props in [("Ticket) RETURN 1",{}),("Ticket",{"acl_allow":["user"]}),
                             ("Ticket",{"entity_id":"new"}),("Ticket",{"x":float("nan")}),
                             ("Ticket",{"x":1<<64}),("Ticket",{"x":b"binary"}),
                             ("Ticket",{"x":"x"*101}),("Ticket",[])]:
            with self.subTest(label=label,props=props), self.assertRaises(ValueError):
                graph.upsert_node(label,"id",props)
        with self.assertRaises(ValueError):
            graph.update_node("Ticket","id",{"x":1},remove=["x"])
        self.assertEqual(graph.load_nodes("Ticket",[]),0)


class ExplicitDriver:
    """Only explicit transactions; the cleanup mirrors Neo4j's transaction context."""
    def __init__(self, *, begin_error=None, commit_error=None):
        self.events = []
        self.begin_error, self.commit_error = begin_error, commit_error
        self.tx = self.Tx(self)

    def session(self, *, database):
        self.events.append(("session", database))
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.events.append("session-close")

    def begin_transaction(self, *, metadata):
        self.events.append(("begin", metadata))
        if self.begin_error:
            raise self.begin_error
        return self.tx

    def execute_write(self, callback):
        raise AssertionError("explicit retained writes must not use managed retries")

    class Tx:
        def __init__(self, driver):
            self.driver, self.closed = driver, False

        def __enter__(self):
            return self

        def __exit__(self, *args):
            if not self.closed:
                self.rollback()
            self.close()

        def commit(self):
            self.driver.events.append("commit")
            self.closed = True  # The real driver also closes when a commit fails.
            if self.driver.commit_error:
                raise self.driver.commit_error

        def rollback(self):
            self.driver.events.append("rollback")
            self.closed = True

        def close(self):
            self.driver.events.append("tx-close")


class QuorumClientTests(unittest.TestCase):
    def test_explicit_metadata_commit_returns_callback_value_once(self):
        driver = ExplicitDriver()
        seen = []
        result = GraphClient(driver, "tickets").write_transaction(
            lambda tx: seen.append(tx) or {"value": 3}, request_id="retained-1_:.")
        self.assertEqual(result, {"value": 3})
        self.assertEqual(seen, [driver.tx])
        self.assertEqual(driver.events, [("session", "tickets"),
            ("begin", {"brainwire_request_id": "retained-1_:."}), "commit", "tx-close", "session-close"])

    def test_callback_error_rolls_back_and_preserves_exception(self):
        driver = ExplicitDriver()
        failure = RuntimeError("callback failed")
        def operation(tx):
            self.assertIs(tx, driver.tx)
            raise failure
        with self.assertRaises(RuntimeError) as caught:
            GraphClient(driver, "tickets").write_transaction(operation, request_id="once")
        self.assertIs(caught.exception, failure)
        self.assertEqual(driver.events[-3:], ["rollback", "tx-close", "session-close"])
        self.assertNotIn("commit", driver.events)

    def test_begin_and_commit_errors_propagate_without_retry_or_result(self):
        for phase, message in [("begin", "AlreadyCommitted"), ("commit", "CommitOutcomeUnknown")]:
            with self.subTest(phase=phase):
                failure = RuntimeError(message)
                driver = ExplicitDriver(**{phase + "_error": failure})
                calls = []
                with self.assertRaises(RuntimeError) as caught:
                    GraphClient(driver, "tickets").write_transaction(
                        lambda tx: calls.append(tx) or "unacknowledged", request_id="same-seed")
                self.assertIs(caught.exception, failure)
                self.assertEqual(len(calls), phase == "commit")
                self.assertEqual(sum(isinstance(event, tuple) and event[0] == "begin" for event in driver.events), 1)
                self.assertEqual(driver.events.count("commit"), phase == "commit")
                self.assertNotIn("rollback", driver.events)
                self.assertEqual(driver.events[-1], "session-close")

    def test_invalid_seed_rejected_before_session_opens(self):
        for seed in [None, 123, "", "a" * 129, "with space", "á", "new\nline", "slash/", "emoji😀"]:
            with self.subTest(seed=seed):
                driver = ExplicitDriver()
                with self.assertRaises(ValueError):
                    GraphClient(driver, "tickets").write_transaction(lambda tx: None, request_id=seed)
                self.assertEqual(driver.events, [])
        driver = ExplicitDriver()
        GraphClient(driver, "tickets").write_transaction(lambda tx: None, request_id="x" * 128)

    def test_request_key_matches_compact_utf8_rust_namespace(self):
        # sha256 of UTF-8 ["clústér","tickets","usuário","seed-1"] without JSON whitespace/ASCII escaping.
        self.assertEqual(request_key("clústér", "tickets", "usuário", "seed-1"),
                         "469587db5e52647c501ea9c0d81a913d1a62babff515e19d30de65e954edf1c8")
        baseline = request_key("cluster", "tickets", "writer", "seed")
        for values in [("other", "tickets", "writer", "seed"), ("cluster", "other", "writer", "seed"),
                       ("cluster", "tickets", "reader", "seed"), ("cluster", "tickets", "writer", "other")]:
            self.assertNotEqual(request_key(*values), baseline)
        for values in [("", "g", "p", "s"), ("c", None, "p", "s"), ("c", "g", 1, "s"),
                       ("c", "g", "p", "with space")]:
            with self.assertRaises(ValueError):
                request_key(*values)


class AclDriver(ExplicitDriver):
    def __init__(self, results=({"processed": 1}, {"updated": True}), **kwargs):
        self.results = iter(results)
        super().__init__(**kwargs)

    def execute_write(self, callback):
        self.events.append("managed")
        with self.tx as tx:
            result = callback(tx)
            tx.commit()
            return result

    class Tx(ExplicitDriver.Tx):
        def run(self, query, **params):
            self.driver.events.append(("run", query, params))
            row = next(self.driver.results)
            class Record:
                def single(self):
                    return row
            return Record()


class AtomicAclClientTests(unittest.TestCase):
    def test_node_patch_and_acl_share_one_transaction_in_both_modes(self):
        for request_id in [None, "acl-operation-1"]:
            with self.subTest(request_id=request_id):
                driver = AclDriver()
                graph = GraphClient(driver, "tickets")
                self.assertTrue(graph.upsert_node_with_acl("Ticket", "t-1",
                    {"subject": "new", "priority": None}, {"allow": ["reader"], "sourceTable": "tickets"},
                    request_id=request_id))
                calls = [event for event in driver.events if isinstance(event, tuple) and event[0] == "run"]
                self.assertEqual(len(calls), 2)
                self.assertTrue(calls[0][1].startswith("MERGE (n:Ticket {entity_id:$id}) SET "))
                self.assertEqual(calls[0][2], {"id": "t-1", "property_0": None, "property_1": "new"})
                self.assertEqual(calls[1][1], "CALL db.setNodeAcl($options) YIELD updated RETURN updated")
                self.assertEqual(calls[1][2], {"options": {"label": "Ticket", "entityId": "t-1",
                    "allow": ["reader"], "sourceTable": "tickets"}})
                self.assertEqual(driver.events.count("commit"), 1)
                self.assertNotIn("rollback", driver.events)
                if request_id is None:
                    self.assertIn("managed", driver.events)
                else:
                    self.assertIn(("begin", {"brainwire_request_id": request_id}), driver.events)
                    self.assertNotIn("managed", driver.events)

    def test_invalid_acl_properties_or_request_seed_fail_before_open(self):
        for props, acl, seed in [({}, {"unknown": []}, None), ({}, [], None),
                ({}, {"allow": object()}, None), ({"acl_allow": []}, {}, None),
                ({"entity_id": "changed"}, {}, None), (None, {}, None), ({}, {}, "bad seed")]:
            with self.subTest(props=props, acl=acl, seed=seed):
                driver = AclDriver()
                with self.assertRaises(ValueError):
                    GraphClient(driver, "tickets").upsert_node_with_acl("Ticket", "t-1", props, acl, request_id=seed)
                self.assertEqual(driver.events, [])

    def test_acl_refusal_or_unexpected_node_count_rolls_back_without_commit(self):
        for rows in [({"processed": 1}, {"updated": False}), ({"processed": 0},), ({"processed": 2},)]:
            for seed in [None, "retained-acl"]:
                with self.subTest(rows=rows, seed=seed):
                    driver = AclDriver(rows)
                    with self.assertRaises(ValueError):
                        GraphClient(driver, "tickets").upsert_node_with_acl("Ticket", "t-1", {}, {}, request_id=seed)
                    self.assertIn("rollback", driver.events)
                    self.assertNotIn("commit", driver.events)
                    self.assertEqual(driver.events[-2:], ["tx-close", "session-close"])

    def test_acl_upsert_does_not_return_success_after_unknown_commit(self):
        failure = RuntimeError("CommitOutcomeUnknown")
        driver = AclDriver(commit_error=failure)
        with self.assertRaises(RuntimeError) as caught:
            GraphClient(driver, "tickets").upsert_node_with_acl("Ticket", "t-1", {}, {}, request_id="retained")
        self.assertIs(caught.exception, failure)
        self.assertEqual(driver.events.count("commit"), 1)

    def test_existing_acl_setter_keeps_managed_api(self):
        driver = AclDriver(({"updated": True},))
        self.assertTrue(GraphClient(driver, "tickets").set_node_acl("Ticket", "t-1", {}))
        self.assertIn("managed", driver.events)
        self.assertEqual(len([event for event in driver.events if isinstance(event, tuple) and event[0] == "run"]), 1)


if __name__ == "__main__":
    unittest.main()
