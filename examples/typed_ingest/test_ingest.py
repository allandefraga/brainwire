import copy
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import MagicMock

from examples.typed_ingest.ingest import MAX_FILE_BYTES, read_json, write_batch


SCHEMA = {
    "version": 1,
    "nodes": {
        "Project": {"name": {"type": "string", "required": True}},
        "Decision": {"active": {"type": "boolean", "required": True}},
    },
    "relationships": {
        "HAS_DECISION": {
            "from": "Project", "to": "Decision",
            "properties": {"weight": {"type": "float", "nullable": True},
                           "position": {"type": "integer"}},
        },
    },
}
BATCH = {
    "nodes": [
        {"label": "Project", "entity_id": "p1", "properties": {"name": "Release"}},
        {"label": "Decision", "entity_id": "d1", "properties": {"active": True}},
    ],
    "relationships": [
        {"type": "HAS_DECISION", "source_id": "p1", "target_id": "d1",
         "properties": {"weight": None, "position": 1}},
    ],
}


class TypedIngestTests(unittest.TestCase):
    def test_checks_entire_batch_before_opening_a_connection(self):
        cases = (
            ("active", 1), ("unknown", "value"), ("active", None),
        )
        for field, value in cases:
            with self.subTest(field=field, value=value):
                batch = copy.deepcopy(BATCH)
                batch["nodes"][1]["properties"][field] = value
                graph = MagicMock()
                with self.assertRaises(ValueError):
                    write_batch(graph, SCHEMA, batch)
                graph.driver.session.assert_not_called()

        for field, value in (("weight", 1), ("weight", float("inf")),
                             ("position", False), ("position", 2**63), ("position", None)):
            with self.subTest(field=field, value=value):
                batch = copy.deepcopy(BATCH)
                batch["relationships"][0]["properties"][field] = value
                graph = MagicMock()
                with self.assertRaises(ValueError):
                    write_batch(graph, SCHEMA, batch)
                graph.driver.session.assert_not_called()

    def test_required_fields_and_duplicate_identities(self):
        for mutate in (
            lambda batch: batch["nodes"][0]["properties"].clear(),
            lambda batch: batch["nodes"].append(batch["nodes"][0]),
            lambda batch: batch["relationships"].append(batch["relationships"][0]),
        ):
            batch = copy.deepcopy(BATCH)
            mutate(batch)
            graph = MagicMock()
            with self.assertRaises(ValueError):
                write_batch(graph, SCHEMA, batch)
            graph.driver.session.assert_not_called()

    def test_strict_schema_and_input_file_limits(self):
        graph = MagicMock()
        schema = copy.deepcopy(SCHEMA)
        schema["relationships"]["HAS_DECISION"]["to"] = "Unknown"
        with self.assertRaises(ValueError):
            write_batch(graph, schema, BATCH)
        graph.driver.session.assert_not_called()

        with TemporaryDirectory() as directory:
            file = Path(directory) / "batch.json"
            file.write_text(json.dumps(BATCH), encoding="utf-8")
            self.assertEqual(read_json(file), BATCH)
            for content in ('{"a": 1, "a": 2}', " " * (MAX_FILE_BYTES + 1)):
                file.write_text(content, encoding="utf-8")
                with self.assertRaises(ValueError):
                    read_json(file)

    def test_write_uses_single_transaction_and_checks_acknowledged_rows(self):
        graph = MagicMock()
        tx = graph.driver.session.return_value.__enter__.return_value.begin_transaction.return_value.__enter__.return_value
        tx.run.return_value.single.return_value = {"processed": 1}
        self.assertEqual(write_batch(graph, SCHEMA, BATCH), {"nodes": 2, "relationships": 1})
        self.assertEqual(tx.run.call_count, 3)
        self.assertIn("MERGE (n:Project", tx.run.call_args_list[0].args[0])
        self.assertIn("[r:HAS_DECISION]", tx.run.call_args.args[0])
        tx.commit.assert_called_once_with()

        tx.reset_mock()
        tx.run.return_value.single.side_effect = [{"processed": 1}, {"processed": 1}, {"processed": 0}]
        with self.assertRaisesRegex(ValueError, "relationship"):
            write_batch(graph, SCHEMA, BATCH)
        tx.commit.assert_not_called()

if __name__ == "__main__":
    unittest.main()
