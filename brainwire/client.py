"""Small Python convenience API over the supported Brainwire Bolt/Cypher path.

Pass an existing Neo4j driver and named database. Missing properties are unchanged;
None removes a property. Node batches are atomic individually, not across a load.
"""
import hashlib
import itertools
import json
import math
import re
from functools import wraps
from contextlib import contextmanager
from uuid import UUID
from collections.abc import Mapping



class BrainwireError(RuntimeError):
    """A recognized Brainwire server failure, retaining the driver exception."""

    def __init__(self, original_error):
        self.original_error = original_error
        self.code = original_error.code
        message = getattr(original_error, "message", None)
        self.message = message if isinstance(message, str) else str(original_error)
        self.rolled_back = None
        super().__init__(self.message)


class MemoryPressureError(BrainwireError):
    """Memory refusal; only a valid server hint supplies rollback and size fields."""

    def __init__(self, original_error):
        super().__init__(original_error)
        self.reason = self.requested_bytes = self.available_bytes = None
        hint = re.search(
            r"\[brainwire\.memory reason=(statement_limit|query_pool|process_limit|cgroup|ingress) "
            r"requested_bytes=([0-9]{1,20}) available_bytes=([0-9]{1,20}) rolled_back=true\]\Z",
            self.message,
        )
        if hint:
            requested, available = int(hint[2]), int(hint[3])
            if requested < 1 << 64 and available < 1 << 64:
                self.reason, self.requested_bytes, self.available_bytes = hint[1], requested, available
                self.rolled_back = True


class CommitOutcomeUnknownError(BrainwireError):
    """The server cannot confirm commit outcome; reconcile before any retry."""


def _server_errors(operation):
    @wraps(operation)
    def invoke(*args, **kwargs):
        try:
            return operation(*args, **kwargs)
        except BrainwireError:
            raise
        except Exception as error:
            code = getattr(error, "code", None)
            if not isinstance(code, str):
                raise
            error_type = {
                "Neo.TransientError.General.MemoryPressure": MemoryPressureError,
                "Neo.ClientError.Transaction.CommitOutcomeUnknown": CommitOutcomeUnknownError,
            }.get(code)
            if error_type is None:
                raise
            raise error_type(error) from error
    return invoke


def _request_id(seed):
    if not isinstance(seed, str) or not re.fullmatch(r"[A-Za-z0-9_:.\-]{1,128}", seed):
        raise ValueError("request_id must be 1..128 ASCII letters/digits or -_:.")
    return seed


def request_key(cluster_id, graph, principal_id, seed):
    """Return the quorum lookup key for an authenticated graph/seed namespace."""
    _request_id(seed)
    if any(not isinstance(value, str) or not value for value in (cluster_id, graph, principal_id)):
        raise ValueError("cluster_id, graph and principal_id must be nonempty strings")
    namespace = json.dumps([cluster_id, graph, principal_id, seed],
                           ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(namespace).hexdigest()


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,127}", value):
        raise ValueError("schema identifiers must be ASCII names of at most 128 characters")
    if value.lower().startswith("acl_"):
        raise ValueError("ACL metadata requires the privileged ACL API")
    return value


def _value(value, depth=0):
    if depth > 32:
        raise ValueError("value nesting exceeds 32")
    if value is None or isinstance(value, (str, bool)):
        return
    if type(value) is int and -(1 << 63) <= value < (1 << 63):
        return
    if type(value) is float and math.isfinite(value):
        return
    if isinstance(value, list):
        for item in value:
            _value(item, depth + 1)
        return
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        for item in value.values():
            _value(item, depth + 1)
        return
    raise ValueError("use null, bool, int64, finite float, string, list or string-keyed map")


def _properties(properties, remove=()):
    if not isinstance(properties, dict) or isinstance(remove, (str, bytes)):
        raise ValueError("properties must be a dict; remove must be a collection of names")
    result = dict(properties)
    for name in remove:
        identifier(name)
        if name in result:
            raise ValueError("cannot set and remove the same property")
        result[name] = None
    for name, value in result.items():
        identifier(name)
        if name == "entity_id":
            raise ValueError("entity_id is immutable")
        _value(value)
    return result


def _entity(value):
    if not isinstance(value, str) or not value:
        raise ValueError("entity_id must be a nonempty string")
    return value


def _acl_options(label, entity_id, acl):
    names = {"allow", "deny", "connectorIds", "sourceTable", "sources"}
    if not isinstance(acl, dict) or not set(acl).issubset(names):
        raise ValueError("invalid ACL metadata fields")
    _value(acl)
    return {"label": identifier(label), "entityId": _entity(entity_id), **acl}


def _set(variable, names, prefix):
    return " SET " + ", ".join(f"{variable}.{name}={prefix}.{name}" for name in names) if names else ""


def _parameters(properties):
    # The current parser supports direct parameters, not $map.property syntax.
    names = sorted(properties)
    return [(name, f"property_{index}") for index, name in enumerate(names)], {
        f"property_{index}":properties[name] for index, name in enumerate(names)
    }


def _batch_rows(chunk, max_rows):
    to_pylist = getattr(chunk, "to_pylist", None)
    if not callable(to_pylist):
        yield chunk
        return

    row_count = getattr(chunk, "num_rows", None)
    slice_batch = getattr(chunk, "slice", None)
    if type(row_count) is int and row_count > max_rows and callable(slice_batch):
        for offset in range(0, row_count, max_rows):
            yield from slice_batch(offset, max_rows).to_pylist()
    else:
        yield from to_pylist()


def _input_rows(source, max_batch_rows):
    """Yield bounded Python mappings from iterables or Arrow-like sources."""
    to_batches = getattr(source, "to_batches", None)
    if callable(to_batches):
        chunks = to_batches(max_chunksize=max_batch_rows)
    else:
        read_next_batch = getattr(source, "read_next_batch", None)
        if callable(read_next_batch):
            def read_batches():
                while True:
                    try:
                        yield read_next_batch()
                    except StopIteration:
                        return
            chunks = read_batches()
        else:
            to_pylist = getattr(source, "to_pylist", None)
            if callable(to_pylist):
                yield from _batch_rows(source, max_batch_rows)
                return
            chunks = source

    for chunk in chunks:
        yield from _batch_rows(chunk, max_batch_rows)


class GraphClient:
    def __init__(self, driver, database, *, batch_size=250, batch_bytes=1 << 20):
        _entity(database)
        if any(type(value) is not int or value <= 0 for value in (batch_size, batch_bytes)):
            raise ValueError("batch bounds must be positive integers")
        self.driver, self.database = driver, database
        self.batch_size, self.batch_bytes = batch_size, batch_bytes

    @_server_errors
    def _write(self, operation):
        # A lost COMMIT reply is ambiguous; managed retries can replay the write.
        with self.driver.session(database=self.database) as session:
            with session.begin_transaction() as tx:
                result = operation(tx)
                tx.commit()
                return result

    @_server_errors
    def run_cypher(self, query, parameters=None):
        """Run one parameterized Cypher statement and return all records as dicts.

        The result is fully consumed before the method returns, so callers do
        not accidentally use a cursor after its session closes. A single
        statement is atomic on the server. This does not retry uncertain writes;
        use ``write_transaction`` with a request ID where the server supports it.
        """
        if not isinstance(query, str) or not query.strip():
            raise ValueError("query must be a nonempty string")
        if parameters is None:
            parameters = {}
        if not isinstance(parameters, Mapping):
            raise ValueError("parameters must be a mapping")
        with self.driver.session(database=self.database) as session:
            return session.run(query, dict(parameters)).data()

    @contextmanager
    def stream_cypher(self, query, parameters=None, *, fetch_size=200):
        """Yield read-only records incrementally inside a ``with`` block.

        ``fetch_size`` bounds the driver's requested pages, not process RSS or
        eager operators in the server. Leaving normally discards unread records
        and checks the terminal summary. The iterator is invalid after exit.
        """
        if not isinstance(query, str) or not query.strip():
            raise ValueError("query must be a nonempty string")
        if parameters is None:
            parameters = {}
        if not isinstance(parameters, Mapping):
            raise ValueError("parameters must be a mapping")
        if type(fetch_size) is not int or fetch_size <= 0:
            raise ValueError("fetch_size must be a positive integer")
        session = self.driver.session(
            database=self.database, fetch_size=fetch_size, default_access_mode="READ"
        )
        try:
            result = _server_errors(session.run)(query, dict(parameters))
            source = iter(result)
            fetch = _server_errors(lambda: next(source).data())

            def records():
                while True:
                    try:
                        record = fetch()
                    except StopIteration:
                        return
                    yield record

            rows = records()
            try:
                yield rows
                # Session.close() otherwise swallows some late server errors.
                _server_errors(result.consume)()
            finally:
                rows.close()
        except BaseException:
            # Preserve the original query or consumer error if cleanup also fails.
            try:
                _server_errors(session.close)()
            except Exception:
                pass
            raise
        else:
            _server_errors(session.close)()

    def schema(self):
        """Return the named graph's catalog snapshot through an authorized read.

        Includes version, node types, relationship signatures and index readiness.
        Describes allowed shapes, not stored instance counts or data visibility.
        Server catalog/query limits apply; this helper does not scan graph records.
        """
        query = ("CALL db.schema() YIELD version, nodes, relationships, indexesReady "
                 "RETURN version, nodes, relationships, indexesReady")
        missing = object()
        with self.stream_cypher(query, fetch_size=1) as rows:
            row = next(rows, missing)
            if (not isinstance(row, dict)
                    or next(rows, missing) is not missing
                    or type(row.get("version")) is not int or row["version"] < 0
                    or type(row.get("indexesReady")) is not bool
                    or any(not isinstance(row.get(name), list)
                           or any(not isinstance(item, dict) for item in row[name])
                           for name in ("nodes", "relationships"))):
                raise ValueError("invalid schema response")
            return {name: row[name] for name in ("version", "nodes", "relationships", "indexesReady")}

    @_server_errors
    def checkpoint(self, name):
        """Create an online native checkpoint of this graph; return its receipt.

        Requires an instance-admin principal and ``server.checkpoint_dir`` on
        the server. ``name`` is a plain directory name, never a path. The
        receipt adds the graph and its ontology version, which only changes
        offline on a standalone server. The copy stays on the server's storage;
        use ``checkpoint_backup.py copy`` to move it to independent storage.
        """
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_\-][A-Za-z0-9_.\-]{0,127}", name):
            raise ValueError("checkpoint name must be 1..128 ASCII letters/digits or -_. not starting with '.'")
        with self.driver.session(database=self.database) as session:
            version = session.run("CALL db.schema() YIELD version RETURN version", {}).data()[0]["version"]
            row = session.run(
                "CALL db.checkpoint($name) YIELD name, path, sequence, tracked "
                "RETURN name, path, sequence, tracked",
                {"name": name},
            ).data()[0]
        return {"graph": self.database, **row, "ontology_version": version}

    @_server_errors
    def write_transaction(self, operation, *, request_id):
        """Run once with a retained request seed on a supported server.

        This requires a quorum leader or an authenticated standalone server with
        its experimental request ledger enabled. No managed retry or leader routing
        occurs; AlreadyCommitted propagates and a recognized unknown-outcome
        server code raises CommitOutcomeUnknownError with the driver cause.
        Return the callback's value only after an acknowledged explicit commit.
        Materialize query results inside operation before its transaction closes.
        """
        _request_id(request_id)
        with self.driver.session(database=self.database) as session:
            with session.begin_transaction(metadata={"brainwire_request_id": request_id}) as tx:
                result = operation(tx)
                tx.commit()
                return result

    def request_status(self, request_id):
        """Read a standalone experimental outcome using a fresh autocommit call.

        Requires the server's authenticated request-ledger mode. The returned
        incarnation describes the current graph history; ``not_recorded`` is
        not confirmation of rollback or permission to replay a write.
        """
        _request_id(request_id)
        rows = self.run_cypher("CALL db.requestStatus($seed)", {"seed": request_id})
        if len(rows) != 1 or not isinstance(rows[0], dict):
            raise ValueError("invalid request-status response")
        row = rows[0]
        if not {"status", "requestId", "incarnation", "sequence"}.issubset(row):
            raise ValueError("invalid request-status response")
        status, sequence = row.get("status"), row.get("sequence")
        valid_sequence = (isinstance(sequence, str) and sequence.isascii()
                          and sequence.isdigit() and len(sequence) <= 20
                          and int(sequence) < 1 << 64)
        if (row.get("requestId") != request_id
                or status not in ("committed", "not_recorded")
                or (status == "committed" and not valid_sequence)
                or (status == "not_recorded" and sequence is not None)):
            raise ValueError("invalid request-status response")
        try:
            UUID(row["incarnation"])
        except (KeyError, ValueError, TypeError, AttributeError) as error:
            raise ValueError("invalid request-status incarnation") from error
        return {name: row[name] for name in ("status", "requestId", "incarnation", "sequence")}

    def upsert_node(self, label, entity_id, properties=None):
        return self.load_nodes(label, [{"entity_id": _entity(entity_id), **_properties({} if properties is None else properties)}])

    def load_nodes(self, label, rows, *, on_batch=None, with_acl=False):
        """Consume an iterable of flat dicts containing entity_id and supplied properties.

        ``with_acl=True`` requires an ``acl`` dict per row (``allow``, ``deny``,
        ``connectorIds``, ``sourceTable``, ``sources``) and replaces each node's
        private ACL metadata in the same transaction as its properties, with one
        ``db.setNodeAcls`` call per batch. That needs graph-write and
        instance-admin access on the server.

        Bounds count compact UTF-8 JSON input bytes, not Bolt wire bytes/RSS.
        For restartable immutable files use the durable-checkpoint importer instead.
        ``on_batch(batch_number, total_rows)`` runs after each successful commit.
        """
        if on_batch is not None and not callable(on_batch):
            raise ValueError("on_batch must be callable")
        label = identifier(label)
        total, batch, size = 0, [], 0
        batches, committed_rows = 0, 0

        def apply(tx, items):
            written = 0
            # Consecutive groups preserve input ordering for repeated identities.
            for names, group in itertools.groupby(items, lambda row: tuple(sorted(row["properties"]))):
                records = [{"entity_id":row["entity_id"], **row["properties"]} for row in group]
                query = f"UNWIND $rows AS row MERGE (n:{label} {{entity_id:row.entity_id}})"
                query += _set("n", names, "row") + " RETURN count(*) AS processed"
                written += tx.run(query, rows=records).single()["processed"]
            if with_acl:
                acls = [row["acl"] for row in items]
                stamped = tx.run("CALL db.setNodeAcls($rows) YIELD updated RETURN updated",
                                 rows=acls).single()["updated"]
                if stamped != len(acls):
                    raise ValueError("ACL batch was not fully acknowledged")
            return written

        def write_batch(items):
            nonlocal total, batches, committed_rows
            total += self._write(lambda tx: apply(tx, items))
            batches += 1
            committed_rows += len(items)
            if on_batch is not None:
                on_batch(batches, committed_rows)

        for row in _input_rows(rows, self.batch_size):
            if not isinstance(row, dict) or "entity_id" not in row:
                raise ValueError("each node row must be a dict containing entity_id")
            if ("acl" in row) != with_acl:
                raise ValueError("rows carry an acl dict exactly when with_acl=True")
            props = _properties({key: value for key, value in row.items() if key not in ("entity_id", "acl")})
            normalized = {"entity_id": _entity(row["entity_id"]), "properties": props}
            if with_acl:
                normalized["acl"] = _acl_options(label, row["entity_id"], row["acl"])
            encoded = json.dumps(normalized, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode()
            if len(encoded) > self.batch_bytes:
                raise ValueError("one node exceeds the batch byte limit")
            if batch and (len(batch) == self.batch_size or size + len(encoded) > self.batch_bytes):
                write_batch(batch)
                batch, size = [], 0
            batch.append(json.loads(encoded))
            size += len(encoded)
        if batch:
            write_batch(batch)
        return total

    def delete_nodes(self, label, entity_ids):
        """Detach-delete node IDs in bounded, individually atomic batches.

        Incident edges are removed with each node. Duplicate IDs within a batch
        are collapsed; a missing or already-deleted ID contributes zero. A
        failed later batch does not roll back earlier batches. This is a direct
        deletion helper, not a source cursor or a whole-snapshot reconciliation.
        """
        label = identifier(label)
        if isinstance(entity_ids, (str, bytes, bytearray, Mapping)):
            raise ValueError("entity_ids must be an iterable of IDs, not a single value or mapping")
        try:
            entity_ids = iter(entity_ids)
        except TypeError as error:
            raise ValueError("entity_ids must be an iterable of IDs") from error
        total, batch, size = 0, [], 0

        def apply(tx, ids):
            # Duplicate IDs in one UNWIND would otherwise inflate count(*) even
            # though the node is physically removed only once.
            ids = list(dict.fromkeys(ids))
            query = (f"UNWIND $ids AS id MATCH (n:{label} {{entity_id:id}}) "
                     "DETACH DELETE n RETURN count(*) AS deleted")
            return tx.run(query, ids=ids).single()["deleted"]

        for entity_id in entity_ids:
            entity_id = _entity(entity_id)
            encoded = json.dumps(entity_id, ensure_ascii=False,
                                 separators=(",", ":")).encode("utf-8")
            if len(encoded) > self.batch_bytes:
                raise ValueError("one entity ID exceeds the batch byte limit")
            if batch and (len(batch) == self.batch_size or size + len(encoded) > self.batch_bytes):
                total += self._write(lambda tx, ids=batch: apply(tx, ids))
                batch, size = [], 0
            batch.append(entity_id)
            size += len(encoded)
            if len(batch) == self.batch_size or size == self.batch_bytes:
                total += self._write(lambda tx, ids=batch: apply(tx, ids))
                batch, size = [], 0
        if batch:
            total += self._write(lambda tx: apply(tx, batch))
        return total

    def delete_edges(self, source_label, relationship, target_label, rows):
        """Delete all physical directed edges for exact endpoint pairs/type.

        Each flat row has only source_id and target_id. Batches are bounded by
        count and compact UTF-8 JSON bytes and atomic individually. Missing
        endpoints or edges count zero; earlier complete batches survive errors.
        On keyed types this deletes every sibling key for each supplied pair;
        use an explicit keyed MATCH/DELETE query to remove one event instead.
        """
        source_label, relationship, target_label = map(
            identifier, (source_label, relationship, target_label))
        if isinstance(rows, (str, bytes, bytearray, Mapping)):
            raise ValueError("rows must be an iterable of endpoint dictionaries")
        try:
            rows = iter(rows)
        except TypeError as error:
            raise ValueError("rows must be an iterable of endpoint dictionaries") from error
        total, batch, size = 0, [], 0

        def apply(tx, items):
            pairs = dict.fromkeys((row["source_id"], row["target_id"]) for row in items)
            records = [{"source_id": source, "target_id": target} for source, target in pairs]
            query = (f"UNWIND $rows AS row MATCH (a:{source_label} {{entity_id:row.source_id}})"
                     f"-[r:{relationship}]->(b:{target_label} {{entity_id:row.target_id}}) "
                     "DELETE r RETURN count(*) AS deleted")
            return tx.run(query, rows=records).single()["deleted"]

        for row in rows:
            if not isinstance(row, dict) or set(row) != {"source_id", "target_id"}:
                raise ValueError("each deletion row must contain only source_id and target_id")
            row = {"source_id": _entity(row["source_id"]), "target_id": _entity(row["target_id"])}
            encoded = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            if len(encoded) > self.batch_bytes:
                raise ValueError("one edge exceeds the batch byte limit")
            if batch and size + len(encoded) > self.batch_bytes:
                total += self._write(lambda tx, items=batch: apply(tx, items))
                batch, size = [], 0
            batch.append(row)
            size += len(encoded)
            if len(batch) == self.batch_size or size == self.batch_bytes:
                total += self._write(lambda tx, items=batch: apply(tx, items))
                batch, size = [], 0
        if batch:
            total += self._write(lambda tx, items=batch: apply(tx, items))
        return total

    def load_edges(self, source_label, relationship, target_label, rows, *, on_batch=None,
                   require_endpoints=True):
        """Merge endpoint/type-identified edges from bounded flat dictionaries.

        Each row contains ``source_id`` and ``target_id`` plus edge properties.
        Each batch is atomic; every endpoint must exist or that batch rolls back.
        ``require_endpoints=False`` instead skips rows whose endpoint is missing;
        the return value then counts only the edges actually merged.
        Omitted edge properties are preserved and supplied nulls remove properties.
        ``on_batch(batch_number, total_rows)`` runs after each successful commit.
        """
        if on_batch is not None and not callable(on_batch):
            raise ValueError("on_batch must be callable")
        source_label = identifier(source_label)
        target_label = identifier(target_label)
        relationship = identifier(relationship)
        total, batch, size = 0, [], 0
        batches, committed_rows = 0, 0

        def apply(tx, items):
            written = 0
            # Keep one UNWIND/MERGE shape per query while preserving row order.
            for names, group in itertools.groupby(items, lambda row: tuple(sorted(row["properties"]))):
                records = [{"source_id": row["source_id"], "target_id": row["target_id"],
                            **row["properties"]} for row in group]
                fields = ", ".join(f"{name}:row.{name}" for name in names)
                edge = f"MERGE (a)-[:{relationship}" + (f" {{{fields}}}" if fields else "") + "]->(b)"
                query = (f"UNWIND $rows AS row MATCH (a:{source_label} {{entity_id:row.source_id}}), "
                         f"(b:{target_label} {{entity_id:row.target_id}}) {edge} RETURN count(*) AS processed")
                count = tx.run(query, rows=records).single()["processed"]
                if require_endpoints and count != len(records):
                    raise ValueError("both endpoints of every edge row must already exist")
                written += count
            return written

        def write_batch(items):
            nonlocal total, batches, committed_rows
            total += self._write(lambda tx: apply(tx, items))
            batches += 1
            committed_rows += len(items)
            if on_batch is not None:
                on_batch(batches, committed_rows)

        for row in _input_rows(rows, self.batch_size):
            if not isinstance(row, dict) or not {"source_id", "target_id"}.issubset(row):
                raise ValueError("each edge row must contain source_id and target_id")
            props = _properties({key: value for key, value in row.items()
                                 if key not in ("source_id", "target_id")})
            normalized = {"source_id": _entity(row["source_id"]),
                          "target_id": _entity(row["target_id"]), "properties": props}
            encoded = json.dumps(normalized, ensure_ascii=False, allow_nan=False,
                                 separators=(",", ":")).encode()
            if len(encoded) > self.batch_bytes:
                raise ValueError("one edge exceeds the batch byte limit")
            if batch and (len(batch) == self.batch_size or size + len(encoded) > self.batch_bytes):
                write_batch(batch)
                batch, size = [], 0
            batch.append(json.loads(encoded))
            size += len(encoded)
        if batch:
            write_batch(batch)
        return total

    def update_node(self, label, entity_id, properties=None, *, remove=()):
        label, entity_id = identifier(label), _entity(entity_id)
        props = _properties({} if properties is None else properties, remove)
        fields, params = _parameters(props)
        query = f"MATCH (n:{label} {{entity_id:$id}})"
        if fields:
            query += " SET " + ", ".join(f"n.{name}=${parameter}" for name, parameter in fields)
        query += " RETURN count(*) AS processed"
        return self._write(lambda tx: tx.run(query, id=entity_id, **params).single()["processed"] == 1)

    def _edge(self, source, relationship, target, properties, remove, require_existing):
        if not isinstance(source, tuple) or not isinstance(target, tuple) or len(source) != 2 or len(target) != 2:
            raise ValueError("endpoints must be (label, entity_id) tuples")
        left, right, relationship = identifier(source[0]), identifier(target[0]), identifier(relationship)
        props = _properties({} if properties is None else properties, remove)
        assignments, values = _parameters(props)
        params = {"source": _entity(source[1]), "target": _entity(target[1]), **values}
        match = f"MATCH (a:{left} {{entity_id:$source}}), (b:{right} {{entity_id:$target}})"
        fields = ", ".join(f"{name}:${parameter}" for name, parameter in assignments)
        edge = f"MERGE (a)-[:{relationship}" + (" {" + fields + "}" if fields else "") + "]->(b) RETURN count(*) AS processed"

        def apply(tx):
            if require_existing:
                query = f"MATCH (a:{left} {{entity_id:$source}})-[:{relationship}]->(b:{right} {{entity_id:$target}}) RETURN count(*) AS processed"
                count = tx.run(query, **params).single()["processed"]
                if count == 0:
                    return False
                if count != 1:
                    raise ValueError("edge update requires one merged relationship per endpoint pair/type")
                query = f"MATCH (a:{left} {{entity_id:$source}})-[r:{relationship}]->(b:{right} {{entity_id:$target}})"
                if assignments:
                    query += " SET " + ", ".join(f"r.{name}=${parameter}" for name, parameter in assignments)
                query += " RETURN count(*) AS processed"
            else:
                query = match + " " + edge
            count = tx.run(query, **params).single()["processed"]
            if count != 1:
                raise ValueError("both edge endpoints must already exist")
            return True
        return self._write(apply)

    def upsert_edge(self, source, relationship, target, properties=None):
        """Create/update an edge using its configured type identity.

        Keyed types require every merge key in properties. Other properties are
        mutable payload; a legacy type uses one edge per endpoint pair.
        """
        return self._edge(source, relationship, target, properties, (), False)

    def update_edge(self, source, relationship, target, properties=None, *, remove=()):
        """Patch the only existing relationship of a pair/type; never create one.

        Multiple keyed siblings require an explicit keyed MATCH/SET query.
        Immutable merge-key changes are rejected by the server.
        """
        return self._edge(source, relationship, target, properties, remove, True)

    def set_node_acl(self, label, entity_id, acl):
        """Replace private metadata; server requires graph-write + instance-admin.

        This is a trusted service operation, not ordinary property editing.
        Omitted ACL fields reset to empty; missing ACL denies restricted readers.
        """
        options = _acl_options(label, entity_id, acl)
        return self._write(lambda tx: tx.run("CALL db.setNodeAcl($options) YIELD updated RETURN updated", options=options).single()["updated"])

    def upsert_node_with_acl(self, label, entity_id, properties, acl, *, request_id=None):
        """Atomically patch a node and replace authoritative private ACL metadata.

        Requires graph-write + instance-admin; supply source-authoritative ACLs.
        Omitted ACL fields reset to empty. A request seed requires quorum serving.
        """
        options = _acl_options(label, entity_id, acl)
        fields, params = _parameters(_properties(properties))
        query = f"MERGE (n:{options['label']} {{entity_id:$id}})"
        if fields:
            query += " SET " + ", ".join(f"n.{name}=${parameter}" for name, parameter in fields)
        query += " RETURN count(*) AS processed"

        def apply(tx):
            if tx.run(query, id=options["entityId"], **params).single()["processed"] != 1:
                raise ValueError("node upsert requires exactly one node")
            if tx.run("CALL db.setNodeAcl($options) YIELD updated RETURN updated",
                      options=options).single()["updated"] is not True:
                raise ValueError("ACL replacement was not acknowledged")
            return True

        return self._write(apply) if request_id is None else self.write_transaction(apply, request_id=request_id)
