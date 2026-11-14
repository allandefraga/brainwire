"""Pure transaction/lease failure tests, without a database or DuckLake."""
import contextlib
import copy
import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest import mock

from examples.python import ducklake_sync as syncer
from examples.source_ingestion import ducklake_incremental as incremental


MAPPING = syncer.Mapping.from_json({"table": "customers", "label": "Customer", "id": "id"})
CONTRACT = syncer._contract_fingerprint(MAPPING, "catalog-v1", "demo", None, True)


class LostCommitReply(ConnectionError):
    pass


class Result:
    def __init__(self, rows=()):
        self.rows = rows

    def data(self):
        return self.rows

    def consume(self):
        return None


class Session:
    def __init__(self, state=None, *, lose_reply=False):
        self.state = state
        self.lose_reply = lose_reply
        self.failure = LostCommitReply("COMMIT response lost")
        self.begins = self.applied = self.managed = self.rollbacks = 0
        self.queries = []

    def session(self, **kwargs):
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def begin_transaction(self):
        self.begins += 1
        return self.Tx(self)

    def execute_write(self, callback):
        self.managed += 1
        for attempt in range(2):
            with self.begin_transaction() as tx:
                value = callback(tx)
                try:
                    tx.commit()
                except LostCommitReply:
                    continue
                return value
        raise self.failure

    class Tx:
        def __init__(self, session):
            self.session = session
            self.state = copy.deepcopy(session.state)
            self.closed = False

        def __enter__(self):
            return self

        def __exit__(self, *args):
            if not self.closed:
                self.session.rollbacks += 1
            self.closed = True

        def run(self, query, **params):
            self.session.queries.append((query, params))
            if "RETURN s.snapshot AS snapshot" in query:
                return Result([] if self.state is None else [dict(self.state)])
            if "CREATE (:ImportCheckpoint" in query:
                self.state = {"snapshot": None, "lease": params["run"], "contract_fingerprint": params["contract"]}
            if "SET s.lease = $run" in query:
                self.state = {**(self.state or {"snapshot": None}), "lease": params["run"]}
            if "SET s.lease = null, s.snapshot = $snapshot" in query:
                self.state.update(snapshot=params["snapshot"], lease=None)
            return Result()

        def commit(self):
            self.session.state = self.state
            self.session.applied += 1
            self.closed = True
            if self.session.lose_reply and self.session.applied == 1:
                raise self.session.failure


class IngestionTransactionTests(unittest.TestCase):
    @staticmethod
    def simple_importer():
        path = Path(__file__).with_name("ingest_customers.py")
        spec = importlib.util.spec_from_file_location("simple_ingest_fixture", path)
        module = importlib.util.module_from_spec(spec)
        # The driver package is needed only by the CLI, not these fake sessions.
        with mock.patch.dict(sys.modules, {"neo4j": types.SimpleNamespace(GraphDatabase=object())}):
            spec.loader.exec_module(module)
        return module

    def test_simple_import_uses_one_transaction_per_acknowledged_batch(self):
        importer = self.simple_importer()
        session = Session()
        self.assertEqual(importer.ingest(session, "customers", [{"entity_id": str(i)} for i in range(3)], 2), 3)
        self.assertEqual((session.begins, session.applied, session.managed), (2, 2, 0))
        self.assertEqual([len(params["rows"]) for _, params in session.queries], [2, 1])

    def test_simple_import_lost_commit_stops_before_second_batch(self):
        session = Session(lose_reply=True)
        with self.assertRaises(LostCommitReply) as error:
            self.simple_importer().ingest(session, "customers", [{"entity_id": str(i)} for i in range(3)], 1)
        self.assertIs(error.exception, session.failure)
        self.assertEqual((session.begins, session.applied, session.managed), (1, 1, 0))
        self.assertEqual(len(session.queries), 1)

    def test_claim_unknown_reply_leaves_lease_and_never_retries(self):
        session = Session(lose_reply=True)
        with self.assertRaises(LostCommitReply) as error:
            syncer.claim(session, "source", "run", contract_fingerprint=CONTRACT)
        self.assertIs(error.exception, session.failure)
        self.assertEqual(session.state, {"snapshot": None, "lease": "run", "contract_fingerprint": CONTRACT})
        self.assertEqual((session.begins, session.applied, session.managed), (1, 1, 0))

    def test_release_unknown_reply_is_not_repeated_or_reported_as_success(self):
        session = Session({"snapshot": 7, "lease": "run", "contract_fingerprint": CONTRACT}, lose_reply=True)
        with self.assertRaises(LostCommitReply) as error:
            syncer.release(session, "source", "run", 7, 8, contract_fingerprint=CONTRACT)
        self.assertIs(error.exception, session.failure)
        self.assertEqual(session.state, {"snapshot": 8, "lease": None, "contract_fingerprint": CONTRACT})
        self.assertEqual((session.begins, session.applied, session.managed), (1, 1, 0))

    def test_successful_claim_and_release_keep_snapshot_contract(self):
        session = Session({"snapshot": 7, "lease": None, "contract_fingerprint": CONTRACT})
        self.assertEqual(syncer.claim(session, "source", "run", contract_fingerprint=CONTRACT), 7)
        syncer.release(session, "source", "run", 7, 8, contract_fingerprint=CONTRACT)
        self.assertEqual(session.state, {"snapshot": 8, "lease": None, "contract_fingerprint": CONTRACT})
        self.assertEqual((session.begins, session.applied, session.managed), (2, 2, 0))
        with self.assertRaisesRegex(RuntimeError, "changed"):
            syncer.release(session, "source", "run", 7, 8, contract_fingerprint=CONTRACT)
        self.assertEqual(session.rollbacks, 1)

    def run_sync(self, *, error=None):
        session = Session()
        release = mock.Mock()
        pool = mock.MagicMock()
        pool.__enter__.return_value = pool
        if error:
            pool.map.side_effect = error
        else:
            pool.map.return_value = [{"upserted": 1, "deleted": 0, "skipped": 0}]
        with mock.patch.object(syncer, "_driver", return_value=session), \
             mock.patch.object(syncer, "auth_from_env", return_value=None), \
             mock.patch.object(syncer, "lake", return_value=contextlib.nullcontext(object())), \
             mock.patch.object(syncer, "latest_snapshot", return_value=8), \
             mock.patch.object(syncer, "validate"), \
             mock.patch.object(syncer, "claim", return_value=7), \
             mock.patch.object(syncer, "release", release), \
             mock.patch.object(syncer, "ProcessPoolExecutor", return_value=pool):
            try:
                value = syncer.sync(MAPPING, lake_target="synthetic", uri="bolt://unused", database="demo",
                                    source_id="source", source_identity="catalog-v1", workers=1)
            except BaseException:
                self.assertEqual(release.call_count, 0, "failed runs must retain their source lease")
                raise
        return value, release

    def test_invalid_batch_size_never_connects_or_claims_a_source_lease(self):
        for batch_size in (0, -1, True, 1.5, "100"):
            with self.subTest(batch_size=batch_size), \
                 mock.patch.object(syncer, "_driver", side_effect=AssertionError("invalid batch opened driver")) as driver, \
                 mock.patch.object(syncer, "claim") as claim:
                with self.assertRaisesRegex(ValueError, "batch_size"):
                    syncer.sync(MAPPING, lake_target="synthetic", uri="bolt://unused",
                                database="demo", source_id="source", source_identity="catalog-v1", workers=1,
                                batch_size=batch_size)
                driver.assert_not_called()
                claim.assert_not_called()

    def test_partition_failure_retains_source_lease(self):
        failure = LostCommitReply("partition outcome unknown")
        with self.assertRaises(LostCommitReply) as error:
            self.run_sync(error=failure)
        self.assertIs(error.exception, failure)

    def test_successful_sync_releases_and_advances_source_once(self):
        result, release = self.run_sync()
        self.assertEqual(result["to"], 8)
        self.assertEqual(release.call_count, 1)
        self.assertEqual(release.call_args.args[-2:], (7, 8))
        self.assertEqual(release.call_args.kwargs, {"contract_fingerprint": CONTRACT})


class RegistrationTests(unittest.TestCase):
    def register(self, session):
        meta = dict(source_id="source", source_identity="identity", from_version=4,
                    from_snapshot_sha256="logical")
        with mock.patch.object(incremental, "validate_artifact", return_value=(meta, "changes", (1, 2, 30))), \
             mock.patch.object(incremental, "fingerprint", return_value=("baseline", (1, 2, 30))), \
             mock.patch.object(incremental, "signature", return_value=(1, 2, 30)), \
             mock.patch.object(incremental, "load_snapshot", return_value=("logical", 1)):
            incremental.initialize_source(session, "demo", "changes.jsonl", "baseline.jsonl", "baseline-id")

    def session(self, *, lose_reply=False, valid=True):
        session = Session(lose_reply=lose_reply)
        def run(tx, query, **params):
            session.queries.append((query, params))
            result = Result()
            if "RETURN s.sha256 AS sha256" in query:
                row = dict(sha256="baseline", next_offset=30,
                           contract_id=incremental.SNAPSHOT_CONTRACT) if valid else None
                result.single = lambda: row
            elif query == incremental.HEAD:
                result.single = lambda: None
            else:
                tx.state = dict(params)
            return result
        # Override only this fake's transaction type; the shared lease fake stays intact.
        class RegistrationTx(Session.Tx):
            def run(self, query, **params):
                return run(self, query, **params)
        session.Tx = RegistrationTx
        return session

    def test_registration_commit_loss_is_not_replayed(self):
        session = self.session(lose_reply=True)
        with self.assertRaises(LostCommitReply):
            self.register(session)
        self.assertEqual((session.begins, session.applied, session.managed), (1, 1, 0))
        self.assertEqual(session.state["version"], 4)
        self.assertEqual(len(session.queries), 3)

    def test_registration_success_and_invalid_baseline(self):
        session = self.session()
        self.register(session)
        self.assertEqual((session.begins, session.applied), (1, 1))
        rejected = self.session(valid=False)
        with self.assertRaisesRegex(ValueError, "baseline graph checkpoint"):
            self.register(rejected)
        self.assertEqual((rejected.applied, rejected.rollbacks), (0, 1))


if __name__ == "__main__":
    unittest.main()
