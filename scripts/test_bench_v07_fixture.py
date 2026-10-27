import argparse
from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
from unittest import mock
import tempfile
import unittest

from scripts import bench_v07_fixture as fixture


class Result:
    def __init__(self, rows): self.rows = rows
    def data(self): return self.rows


class FakeTx:
    def __init__(self, state, mode, metadata):
        self.state, self.mode, self.metadata = state, mode, metadata
    def __enter__(self): return self
    def __exit__(self, *unused): pass
    def commit(self): self.state["commits"] += 1
    def run(self, query, params):
        n = self.state["rows"]
        if query == "RETURN 1 AS ready": return Result([{"ready": 1}])
        if query == fixture.SCHEMA:
            return Result([{"version": 1,
                            "nodes": [{"label": label} for label in [*fixture.LABELS, "Pessoa"]],
                            "relationships": [{"source": source, "name": edge, "target": target}
                                              for source, edge, target in fixture.EDGES]}])
        if query.startswith("CALL db.setNodeAcls"):
            assert self.mode == "WRITE" and self.metadata == {}
            self.state["batches"].append(params["rows"])
            return Result([{"updated": len(params["rows"]) - int(self.state.get("bad_ack", False))}])
        policy = self.metadata.get("brainwire_visibility")
        if policy is not None:
            mode = "allow_all" if policy.get("allow_all") else "restricted"
            expected = fixture.expected_results(n, self.state.get("acl_mode", "balanced"))[mode]
            if "group_value" in query: return Result(expected["top_n"])
            count = expected["cursor_rows"] if "MATCH (n:Usuario)" in query else expected["count_pairs"]
            return Result([{"count": count}])
        if query == "MATCH (n) RETURN count(n) AS count": return Result([{"count": 4*n+2}])
        if "RETURN count" in query: return Result([{"count": n}])
        raise AssertionError(query)


class FakeSession:
    def __init__(self, state, mode): self.state, self.mode = state, mode
    def __enter__(self): return self
    def __exit__(self, *unused): pass
    def begin_transaction(self, metadata, timeout):
        self.state["metadata"].append(metadata)
        return FakeTx(self.state, self.mode, metadata)


class FakeDriver:
    def __init__(self, state): self.state = state
    def __enter__(self): return self
    def __exit__(self, *unused): pass
    def session(self, database, default_access_mode):
        assert database == "bench"
        return FakeSession(self.state, default_access_mode)


class FakeServer:
    def __init__(self, state, binary, env, directory):
        self.state = state
        state["server_stopped"] = False
        assert env["BRAINWIRE_SERVER_LISTEN"].startswith("127.0.0.1:")
        policy = json.loads(Path(env["BRAINWIRE_SERVER_AUTH_FILE"]).read_text())
        assert len(policy["principals"]) == 2
    def poll(self): return None
    def stop(self): self.state["server_stopped"] = True


class FixtureTests(unittest.TestCase):
    def source(self, root, rows=9):
        source = root/"source"
        graph = source/"data"/"bench"
        graph.mkdir(parents=True)
        (graph/"LOCK").write_bytes(b"")
        (graph/"CURRENT").write_bytes(b"synthetic-current")
        receipt = {"kind": "embedded synthetic benchmark", "status": "completed_embedded_only",
                   "dataset": {"rows_per_label": rows, "fixture_nodes": 4*rows,
                               "fixture_edges": 4*rows, "labels": list(fixture.LABELS)},
                   "phases": [{"name": "check", "status": "ok", "records": [{"phase": "check", "status": "ok"}]}]}
        path = source/"receipt.json"
        path.write_text(json.dumps(receipt))
        return path, graph
    def args(self, root, receipt):
        binary = root/"fake-binary"
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o700)
        return argparse.Namespace(source_receipt=receipt, confirm_source_synthetic=True,
                                  binary=binary, source_revision="a"*40, output=root/"output", batch_size=4,
                                  acl_mode="balanced")
    def test_exact_expected_results_cover_small_and_million_row_boundaries(self):
        for count in [1, 4, 8, 9, 1_000_000]:
            result = fixture.expected_results(count)
            self.assertEqual(sum(row["count"] for row in result["allow_all"]["top_n"]), count)
            visible = sum(i % 8 < 4 for i in range(count))
            self.assertEqual(result["restricted"]["cursor_rows"], visible)
            self.assertEqual(sum(row["count"] for row in result["restricted"]["top_n"]), visible)
    def test_full_cursor_mode_has_a_million_visible_sources_and_half_visible_pairs(self):
        result=fixture.expected_results(1_000_000,"full-cursor")
        self.assertEqual(result["restricted"]["cursor_rows"],1_000_000)
        self.assertEqual(result["restricted"]["count_pairs"],500_000)
        self.assertEqual(result["restricted"]["top_n"],
                         [{"group_value":brand,"count":125_000} for brand in fixture.BRANDS])

    def test_full_cursor_preparation_stamps_all_sources_but_only_half_targets_visible(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); receipt,_=self.source(root); args=self.args(root,receipt)
            args.acl_mode="full-cursor"
            state={"rows":9,"batches":[],"metadata":[],"commits":0,"acl_mode":"full-cursor"}
            with redirect_stdout(io.StringIO()):
                fixture.prepare(args,lambda *a,**k:FakeDriver(state),lambda *a:FakeServer(state,*a),
                                lambda *a:{"status":"ok"},lambda:"127.0.0.1:19087")
            rows=[row for batch in state["batches"] for row in batch]
            users=[row for row in rows if row["label"]=="Usuario"]
            cars=[row for row in rows if row["label"]=="Carro"]
            self.assertTrue(all(row["allow"]==[fixture.READER] for row in users))
            self.assertEqual(sum(row["allow"]==[fixture.READER] for row in cars),5)
            manifest=json.loads((args.output/"manifest.json").read_text())
            prepared=json.loads((args.output/"prepared.json").read_text())
            self.assertEqual(manifest["fixture_acl_mode"],"full-cursor")
            self.assertEqual(prepared["acl_mode"],"full-cursor")
            self.assertEqual(manifest["expected"]["restricted"]["cursor_rows"],9)
            self.assertEqual(manifest["expected"]["restricted"]["count_pairs"],5)

    def test_unknown_acl_mode_is_refused(self):
        with self.assertRaises(ValueError): fixture.expected_results(8,"unknown")
        state={"rows":8,"batches":[],"metadata":[],"commits":0}
        with self.assertRaises(ValueError): fixture.stamp(FakeDriver(state),"bench",8,4,"unknown")
        self.assertEqual(state["batches"],[])

    def test_prepare_copies_only_to_new_graph_and_freezes_reader_after_verified_stamps(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); receipt, source = self.source(root)
            args = self.args(root, receipt)
            state = {"rows": 9, "batches": [], "metadata": [], "commits": 0}
            driver = lambda *a, **k: FakeDriver(state)
            server = lambda *a: FakeServer(state, *a)
            def check(*unused):
                self.assertTrue(state["server_stopped"])
                return {"status": "ok"}
            with redirect_stdout(io.StringIO()):
                self.assertEqual(fixture.prepare(args, driver, server, check, lambda: "127.0.0.1:19087"), 0)
            manifest = json.loads((args.output/"manifest.json").read_text())
            self.assertIs(type(manifest["schema_version"]), int)
            self.assertTrue(manifest["frozen"])
            self.assertEqual(manifest["expected"]["restricted"]["count_pairs"], 5)
            batches = state["batches"]
            self.assertEqual(sum(map(len, batches)), 18)
            self.assertTrue(all(len(batch) <= 4 for batch in batches))
            entries = [entry for batch in batches for entry in batch]
            self.assertEqual(entries[0]["allow"], [fixture.READER])
            self.assertEqual(entries[4]["allow"], [fixture.HIDDEN])
            policy = json.loads((args.output/"auth"/"policy.json").read_text())
            self.assertEqual(len(policy["principals"]), 1)
            self.assertEqual(policy["principals"][0]["graphs"], {"bench": "read"})
            self.assertFalse(policy["principals"][0]["instance_admin"])
            client = json.loads((args.output/"client-env.json").read_text())
            prepared = (args.output/"prepared.json").read_text()
            self.assertNotIn(client["BRAINWIRE_BENCH_TOKEN"], prepared)
            self.assertNotIn(client["BRAINWIRE_BENCH_TOKEN"], json.dumps(manifest))
            self.assertEqual(stat.S_IMODE((args.output/"client-env.json").stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(args.output.stat().st_mode), 0o700)
            self.assertEqual((source/"CURRENT").read_bytes(), b"synthetic-current")
            self.assertEqual(json.loads(prepared)["additional_nodes_outside_four_label_fixture"], 2)
            binary, settings = fixture.measurement_server(args.output)
            self.assertEqual(binary, args.binary.resolve())
            settings["BRAINWIRE_SERVER_DATA_DIR"] = str(source.parent)
            config_path = args.output/"server.json"
            config = json.loads(config_path.read_text())
            config["environment"] = settings
            config_path.write_text(json.dumps(config))
            with self.assertRaises(ValueError): fixture.measurement_server(args.output)
    def test_missing_confirmation_and_source_symlink_refuse_before_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); receipt, graph=self.source(root)
            args=self.args(root,receipt); args.confirm_source_synthetic=False
            with self.assertRaises(ValueError): fixture.prepare(args)
            self.assertFalse(args.output.exists())
            args.confirm_source_synthetic=True
            (graph/"bad-link").symlink_to(graph/"CURRENT")
            with self.assertRaises(ValueError): fixture.prepare(args)
            self.assertFalse(args.output.exists())
    def test_source_parent_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); receipt,_=self.source(root)
            data=receipt.parent/"data"
            data.rename(receipt.parent/"real-data")
            data.symlink_to(receipt.parent/"real-data", target_is_directory=True)
            with self.assertRaises(ValueError): fixture.source_fixture(receipt,True)

    def test_source_lock_stays_exclusive_during_and_after_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); _, source=self.source(root)
            destination=root/"copy"
            probe = """import errno,fcntl,os,sys
fd=os.open(sys.argv[1],os.O_RDWR)
try:
    fcntl.lockf(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
except OSError as error:
    sys.exit(0 if error.errno in (errno.EACCES,errno.EAGAIN) else 3)
else:
    sys.exit(2)
"""
            def check_lock():
                result=subprocess.run([sys.executable,"-c",probe,str(source/"LOCK")],capture_output=True)
                self.assertEqual(result.returncode,0,result.stderr)
            original=fixture.shutil.copytree
            def copying(*args,**kwargs):
                def each(src,dst):
                    self.assertNotEqual(Path(src),source/"LOCK")
                    check_lock()
                    result=fixture.shutil.copy2(src,dst)
                    check_lock()
                    return result
                kwargs["copy_function"]=each
                return original(*args,**kwargs)
            with mock.patch.object(fixture.shutil,"copytree",side_effect=copying):
                with fixture.locked_copy(source,destination):
                    check_lock()
            self.assertTrue((destination/"LOCK").is_file())
            self.assertNotEqual((source/"LOCK").stat().st_ino,(destination/"LOCK").stat().st_ino)
            # The source is available again when the context ends.
            result=subprocess.run([sys.executable,"-c",probe,str(source/"LOCK")],capture_output=True)
            self.assertEqual(result.returncode,2,result.stderr)

    def test_hard_link_alias_of_source_lock_is_rejected_before_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); _, source=self.source(root)
            os.link(source/"LOCK",source/"lock-alias")
            destination=root/"copy"
            with self.assertRaisesRegex(ValueError,"hard-link aliases"):
                with fixture.locked_copy(source,destination):
                    self.fail("hard-linked source lock was accepted")
            self.assertFalse(destination.exists())
            self.assertEqual((source/"LOCK").stat().st_nlink,2)

    def test_existing_output_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); receipt,_=self.source(root); args=self.args(root,receipt)
            args.output.mkdir(); (args.output/"sentinel").write_text("keep")
            with self.assertRaises(FileExistsError): fixture.prepare(args)
            self.assertEqual((args.output/"sentinel").read_text(),"keep")
    def test_bad_acknowledgment_stops_only_own_server_and_never_freezes_manifest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); receipt,_=self.source(root); args=self.args(root,receipt)
            state={"rows":9,"batches":[],"metadata":[],"commits":0,"bad_ack":True}
            with self.assertRaises(ValueError):
                fixture.prepare(args,lambda *a,**k:FakeDriver(state),lambda *a:FakeServer(state,*a),lambda *a:{"status":"ok"}, lambda: "127.0.0.1:19087")
            self.assertTrue(state["server_stopped"])
            self.assertFalse((args.output/"manifest.json").exists())
            self.assertEqual(json.loads((args.output/"prepared.json").read_text())["status"],"failed")
    def test_failed_consistency_check_never_freezes_manifest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); receipt,_=self.source(root); args=self.args(root,receipt)
            state={"rows":9,"batches":[],"metadata":[],"commits":0}
            def fail(*unused): raise RuntimeError("check failed")
            with self.assertRaises(RuntimeError):
                fixture.prepare(args,lambda *a,**k:FakeDriver(state),lambda *a:FakeServer(state,*a),fail, lambda: "127.0.0.1:19087")
            self.assertTrue(state["server_stopped"])
            self.assertFalse((args.output/"manifest.json").exists())


if __name__ == "__main__": unittest.main()
