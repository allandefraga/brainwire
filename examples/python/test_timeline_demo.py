import unittest
from unittest.mock import MagicMock

from examples.python.timeline_demo import event_id, ontology, read_events, record_event


class TimelineTest(unittest.TestCase):
    def driver(self, existing=None):
        driver = MagicMock()
        tx = driver.session.return_value.__enter__.return_value.begin_transaction.return_value.__enter__.return_value
        tx.run.return_value.single.side_effect = ([{"found": 1}, existing, {"linked": 1}]
                                                if existing is not None else
                                                [{"found": 1}, None, {"written": 1}, {"linked": 1}])
        return driver, tx

    def record(self, driver, **kwargs):
        return record_event(driver, "demo", ("Ticket", "ticket-1"), "opened-1",
                            "opened", "2026-11-03T10:00:00+00:00", {"owner": "Ada"}, **kwargs)

    def test_identity_is_stable_and_namespaces_subject_and_event(self):
        first = event_id(("Ticket", "ticket-1"), "opened-1")
        self.assertEqual(first, event_id(("Ticket", "ticket-1"), "opened-1"))
        self.assertNotEqual(first, event_id(("Ticket", "ticket-1"), "closed-1"))
        self.assertNotEqual(first, event_id(("Ticket", "ticket-2"), "opened-1"))
        self.assertNotEqual(event_id(("A", "b:c"), "d"), event_id(("A", "b"), "c:d"))

    def test_event_and_link_commit_together_and_replay_uses_same_identity(self):
        driver, tx = self.driver()
        first = self.record(driver)
        tx.commit.assert_called_once()
        params = tx.run.call_args_list[2].args[1]
        self.assertEqual(params["event"], first)
        self.assertEqual(params["occurred_at"], "2026-11-03T10:00:00+00:00")
        self.assertEqual(params["payload_json"], '{"owner":"Ada"}')
        existing = {k: params[k] for k in ("kind", "occurred_at", "payload_json")}
        replay, replay_tx = self.driver(existing)
        self.assertEqual(self.record(replay), first)
        self.assertEqual(replay_tx.run.call_count, 3)
        self.assertNotIn("SET", replay_tx.run.call_args_list[2].args[0])

    def test_changed_replay_and_missing_subject_fail_before_writes(self):
        driver, tx = self.driver({"kind": "closed", "occurred_at": "x", "payload_json": "{}"})
        with self.assertRaisesRegex(ValueError, "different"):
            self.record(driver)
        tx.commit.assert_not_called()
        self.assertEqual(tx.run.call_count, 2)
        driver, tx = self.driver()
        tx.run.return_value.single.side_effect = [{"found": 0}]
        with self.assertRaisesRegex(ValueError, "subject"):
            self.record(driver)
        tx.commit.assert_not_called()
        self.assertEqual(tx.run.call_count, 1)

    def test_link_failure_does_not_commit_or_retry(self):
        driver, tx = self.driver()
        tx.run.side_effect = [MagicMock(single=lambda: {"found": 1}),
                              MagicMock(single=lambda: None),
                              MagicMock(single=lambda: {"written": 1}), OSError("connection lost")]
        with self.assertRaises(OSError):
            self.record(driver)
        tx.commit.assert_not_called()
        self.assertEqual(tx.run.call_count, 4)

    def test_timezone_and_input_validation_precede_connection(self):
        driver, tx = self.driver()
        record_event(driver, "demo", ("Ticket", "ticket-1"), "e", "opened",
                     "2026-11-03T11:00:00+01:00", {})
        self.assertEqual(tx.run.call_args_list[2].args[1]["occurred_at"], "2026-11-03T10:00:00+00:00")
        for timestamp in ("yesterday", "2026-11-03T10:00:00"):
            driver = MagicMock()
            with self.assertRaises(ValueError):
                record_event(driver, "demo", ("Ticket", "ticket-1"), "e", "opened", timestamp, {})
            driver.session.assert_not_called()
        for subject in (("Ticket); DELETE n", "x"), ("Ticket", "")):
            with self.assertRaises(ValueError):
                event_id(subject, "e")

    def test_schema_declares_separate_event_nodes(self):
        schema = ontology("Ticket")
        self.assertEqual([n["label"] for n in schema["nodes"]], ["Ticket", "TimelineEvent"])
        self.assertEqual(schema["edges"][0]["from"], {"label": "Ticket"})
        self.assertEqual(schema["edges"][0]["to"], {"label": "TimelineEvent"})

    def test_read_returns_a_chronological_page_with_a_stable_tie_breaker(self):
        driver = MagicMock()
        session = driver.session.return_value.__enter__.return_value
        records = [{"id": "timeline:a", "kind": "opened", "occurred_at": "2026-11-03T10:00:00+00:00",
                    "payload_json": "{}"}]
        session.run.return_value = iter(records)
        self.assertEqual(read_events(driver, "demo", ("Ticket", "ticket-1"), limit=20), records)
        query, params = session.run.call_args.args
        self.assertIn("MATCH (s:Ticket {entity_id:$subject})-[:HAS_EVENT]->(e:TimelineEvent)", query)
        self.assertIn("ORDER BY e.occurred_at, e.entity_id LIMIT $limit", query)
        self.assertEqual(params, {"subject": "ticket-1", "limit": 20})
        driver.session.assert_called_once_with(database="demo")

    def test_read_validation_precedes_connection(self):
        invalid = [(("Ticket", "ticket-1"), 0), (("Ticket", "ticket-1"), 101),
                   (("Ticket", "ticket-1"), True), (("Ticket", "ticket-1"), 1.5),
                   (("Ticket); DELETE n", "ticket-1"), 20), (("Ticket", ""), 20),
                   (("TimelineEvent", "event-1"), 20), (("Ticket",), 20)]
        for subject, limit in invalid:
            with self.subTest(subject=subject, limit=limit):
                driver = MagicMock()
                with self.assertRaises(ValueError):
                    read_events(driver, "demo", subject, limit=limit)
                driver.session.assert_not_called()


if __name__ == "__main__":
    unittest.main()
