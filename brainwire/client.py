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



class BatchTooLargeError(ValueError):
    """One input item cannot fit in the configured client-side byte budget."""

    def __init__(self, item_type, limit_bytes):
        self.item_type = item_type
        self.limit_bytes = limit_bytes
        label = 'entity ID' if item_type == 'entity_id' else item_type
        super().__init__(f'one {label} exceeds the batch byte limit')

    def __reduce__(self):
        return type(self), (self.item_type, self.limit_bytes)


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
            r"\[brainwire\.memory reason=(statement_limit|query_pool|process_limit|process_monitor|cgroup|ingress) "
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


def _value(value, depth=0, budget=None):
    if depth > 32:
        raise ValueError("value nesting exceeds 32")
    if (value is None or isinstance(value, (str, bool)) or
            (type(value) is int and -(1 << 63) <= value < (1 << 63)) or
            (type(value) is float and math.isfinite(value))):
        if budget is not None:
            if type(value) is str and len(value) < 4096 and len(value) * 6 + 2 <= budget[0]:
                budget[0] -= len(value) + 2
            else:
                _charge_batch_value(budget, value)
        return
    if isinstance(value, list):
        if type(value) is not list:
            for item in value:
                _value(item, depth + 1)
            return
        _charge_batch_container(budget, len(value), mapping=False)
        for item in value:
            _value(item, depth + 1, budget)
        return
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        if type(value) is not dict:
            for item in value.values():
                _value(item, depth + 1)
            return
        _charge_batch_container(budget, len(value), mapping=True)
        for key, item in value.items():
            _charge_batch_value(budget, key)
            _value(item, depth + 1, budget)
        return
    raise ValueError("use null, bool, int64, finite float, string, list or string-keyed map")


def _properties(properties, remove=(), *, budget=None):
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
        _value(value, budget=budget)
    return result


def _declared_relationship_key(tx, source_label, relationship, target_label):
    schema = tx.run("CALL db.schema() YIELD relationships RETURN relationships").single()
    relationships = schema.get("relationships") if isinstance(schema, Mapping) else None
    if not isinstance(relationships, list) or any(not isinstance(item, dict) for item in relationships):
        raise ValueError("invalid schema response")
    matches = [item for item in relationships
               if (item.get("name"), item.get("source"), item.get("target"))
               == (relationship, source_label, target_label)]
    fields = matches[0].get("mergeKeyFields") if len(matches) == 1 else None
    if (not isinstance(fields, list) or not fields
            or any(not isinstance(field, str) for field in fields)
            or len(set(fields)) != len(fields)):
        raise ValueError("key must contain the complete declared relationship key")
    return tuple(sorted(fields))


def _entity(value):
    if not isinstance(value, str) or not value:
        raise ValueError("entity_id must be a nonempty string")
    return value


def _batch_budget_remaining(value, remaining):
    # Subtract a lower bound; exact compact JSON size is checked afterward.
    if isinstance(value, str):
        if type(value) is not str:
            return remaining
        minimum = len(value) + 2
        if minimum > remaining:
            return -1
        if len(value) >= 4096 or len(value) * 6 + 2 > remaining:
            if value.isascii() and not re.search(r'["\\\x00-\x1f]', value):
                return remaining - minimum
            size = 2
            for start in range(0, len(value), 4096):
                part = json.dumps(value[start:start + 4096], ensure_ascii=False).encode('utf-8')
                size += len(part) - 2
                if size > remaining:
                    return -1
            return remaining - size
        return remaining - minimum
    return remaining - 1


def _charge_batch_value(budget, value):
    if budget is not None:
        budget[0] = _batch_budget_remaining(value, budget[0])
        if budget[0] < 0:
            raise BatchTooLargeError(budget[1], budget[2])


def _charge_batch_container(budget, length, *, mapping):
    if budget is None:
        return
    minimum = ((5 if mapping else 2) * length + 1) if length else 2
    if minimum > budget[0]:
        raise BatchTooLargeError(budget[1], budget[2])
    budget[0] -= 2 + max(0, length - 1) + (length if mapping else 0)


def _has_exact_keys(row, names):
    if not isinstance(row, dict):
        return False
    if type(row) is not dict:
        return set(row) == set(names)
    return len(row) == len(names) and all(name in row for name in names)


def _acl_options(label, entity_id, acl, *, budget=None):
    names = {"allow", "deny", "connectorIds", "sourceTable", "sources"}
    if not isinstance(acl, dict):
        raise ValueError("invalid ACL metadata fields")
    if type(acl) is dict:
        invalid = len(acl) > len(names) or any(name not in names for name in acl)
    else:
        invalid = not set(acl).issubset(names)
    if invalid:
        raise ValueError("invalid ACL metadata fields")
    _value(acl, budget=budget)
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
        receipt adds the graph and a separately sampled ontology version. An
        online catalog change can make that version older than the checkpoint.
        The copy stays on the server's storage;
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

    def load_nodes(self, label, rows, *, on_batch=None, with_acl=False, request_id=None):
        """Consume an iterable of flat dicts containing entity_id and supplied properties.

        ``with_acl=True`` requires an ``acl`` dict per row (``allow``, ``deny``,
        ``connectorIds``, ``sourceTable``, ``sources``) and replaces each node's
        private ACL metadata in the same transaction as its properties, with one
        ``db.setNodeAcls`` call per batch. That needs graph-write and
        instance-admin access on the server.

        Bounds count compact UTF-8 JSON input bytes, not Bolt wire bytes/RSS.
        For restartable immutable files use the durable-checkpoint importer instead.
        ``on_batch(batch_number, total_rows)`` runs after each successful commit.
        If that callback raises, the batch has already committed.
        Without ``request_id``, a full batch commits before the next row.

        ``request_id`` identifies one bounded batch on a supported server. All
        input is validated before a transaction opens; exceeding either batch
        bound raises without writing a prefix. Empty input sends no request.
        Requires quorum or opt-in authenticated standalone request-ledger mode.
        No retry occurs; use the appropriate outcome lookup after uncertainty,
        and never interpret ``not_recorded`` as permission to replay.
        """
        if on_batch is not None and not callable(on_batch):
            raise ValueError("on_batch must be callable")
        if request_id is not None:
            _request_id(request_id)
            if with_acl:
                raise ValueError("identified node batches do not support with_acl")
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
            operation = lambda tx: apply(tx, items)
            total += (self._write(operation) if request_id is None else
                      self.write_transaction(operation, request_id=request_id))
            batches += 1
            committed_rows += len(items)
            if on_batch is not None:
                on_batch(batches, committed_rows)

        for row in _input_rows(rows, self.batch_size):
            if not isinstance(row, dict) or "entity_id" not in row:
                raise ValueError("each node row must be a dict containing entity_id")
            if ("acl" in row) != with_acl:
                raise ValueError("rows carry an acl dict exactly when with_acl=True")
            # Each valid property needs at least a quoted name, colon and value.
            if type(row) is dict and 5 * (len(row) - (2 if with_acl else 1)) > self.batch_bytes:
                raise BatchTooLargeError("node", self.batch_bytes)
            budget = [self.batch_bytes, "node", self.batch_bytes]
            props = _properties({key: value for key, value in row.items() if key not in ("entity_id", "acl")}, budget=budget)
            normalized = {"entity_id": _entity(row["entity_id"]), "properties": props}
            _charge_batch_value(budget, normalized["entity_id"])
            if with_acl:
                normalized["acl"] = _acl_options(label, row["entity_id"], row["acl"], budget=budget)
            encoded = json.dumps(normalized, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode()
            if len(encoded) > self.batch_bytes:
                raise BatchTooLargeError("node", self.batch_bytes)
            if batch and (len(batch) == self.batch_size or size + len(encoded) > self.batch_bytes):
                if request_id is not None:
                    raise ValueError("identified input must fit in one bounded batch")
                write_batch(batch)
                batch, size = [], 0
            batch.append(json.loads(encoded))
            size += len(encoded)
            if request_id is None and (len(batch) == self.batch_size or size == self.batch_bytes):
                write_batch(batch)
                batch, size = [], 0
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
            budget = [self.batch_bytes, "entity_id", self.batch_bytes]
            _charge_batch_value(budget, entity_id)
            encoded = json.dumps(entity_id, ensure_ascii=False,
                                 separators=(",", ":")).encode("utf-8")
            if len(encoded) > self.batch_bytes:
                raise BatchTooLargeError("entity_id", self.batch_bytes)
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
        use ``delete_edge(..., key=...)`` to remove one event instead.
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
            if not _has_exact_keys(row, ("source_id", "target_id")):
                raise ValueError("each deletion row must contain only source_id and target_id")
            row = {"source_id": _entity(row["source_id"]), "target_id": _entity(row["target_id"])}
            budget = [self.batch_bytes, "edge", self.batch_bytes]
            _charge_batch_value(budget, row["source_id"])
            _charge_batch_value(budget, row["target_id"])
            encoded = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            if len(encoded) > self.batch_bytes:
                raise BatchTooLargeError("edge", self.batch_bytes)
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

    def delete_keyed_edges(self, source_label, relationship, target_label, rows):
        """Delete selected immutable-key events in bounded atomic batches.

        Each row contains only source_id, target_id and a key dictionary with
        every declared immutable key field as a nonempty string. Missing events
        count zero; sibling keys and nodes are preserved. Duplicate identities
        collapse within a batch. Earlier batches survive later failures.
        Each batch validates its catalog and ambiguity in its write transaction;
        it uses up to three queries and does not automatically retry.
        """
        source_label, relationship, target_label = map(
            identifier, (source_label, relationship, target_label))
        if isinstance(rows, (str, bytes, bytearray, Mapping)):
            raise ValueError("rows must be an iterable of keyed endpoint dictionaries")
        try:
            rows = iter(rows)
        except TypeError as error:
            raise ValueError("rows must be an iterable of keyed endpoint dictionaries") from error
        total, batch, size = 0, [], 0

        def apply(tx, items):
            names = _declared_relationship_key(tx, source_label, relationship, target_label)
            key_names = set(names)
            if any(set(item["key"]) != key_names for item in items):
                raise ValueError("key must contain the complete declared relationship key")
            identities = dict.fromkeys((item["source_id"], item["target_id"],
                                       *(item["key"][name] for name in names)) for item in items)
            records = [{"source_id": identity[0], "target_id": identity[1],
                        **{f"key_{index}": value for index, value in enumerate(identity[2:])}}
                       for identity in identities]
            fields = ", ".join(f"{name}:row.key_{index}" for index, name in enumerate(names))
            pattern = (f"UNWIND $rows AS row MATCH (a:{source_label} {{entity_id:row.source_id}})"
                       f"-[r:{relationship} {{{fields}}}]->(b:{target_label} {{entity_id:row.target_id}})")
            ambiguous = tx.run(pattern + " WITH row, count(*) AS matched WHERE matched > 1 "
                               "RETURN count(*) AS ambiguous", rows=records).single()["ambiguous"]
            if ambiguous != 0:
                raise ValueError("edge mutation requires one merged relationship per selection")
            count = tx.run(pattern + " DELETE r RETURN count(*) AS deleted", rows=records).single()["deleted"]
            if count > len(records):
                raise ValueError("edge mutation requires one merged relationship per selection")
            return count

        for row in rows:
            if not _has_exact_keys(row, ("source_id", "target_id", "key")):
                raise ValueError("each keyed deletion row must contain only source_id, target_id and key")
            key = row["key"]
            if (not isinstance(key, dict) or not key
                    or any(not isinstance(value, str) or not value for value in key.values())):
                raise ValueError("key must be a nonempty dictionary of nonempty strings")
            if type(key) is dict and 5 * len(key) > self.batch_bytes:
                raise BatchTooLargeError("edge", self.batch_bytes)
            budget = [self.batch_bytes, "edge", self.batch_bytes]
            normalized = {"source_id": _entity(row["source_id"]), "target_id": _entity(row["target_id"]),
                          "key": _properties(key, budget=budget)}
            _charge_batch_value(budget, normalized["source_id"])
            _charge_batch_value(budget, normalized["target_id"])
            encoded = json.dumps(normalized, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            if len(encoded) > self.batch_bytes:
                raise BatchTooLargeError("edge", self.batch_bytes)
            if batch and size + len(encoded) > self.batch_bytes:
                total += self._write(lambda tx, items=batch: apply(tx, items))
                batch, size = [], 0
            batch.append(normalized)
            size += len(encoded)
            if len(batch) == self.batch_size or size == self.batch_bytes:
                total += self._write(lambda tx, items=batch: apply(tx, items))
                batch, size = [], 0
        if batch:
            total += self._write(lambda tx, items=batch: apply(tx, items))
        return total

    def load_edges(self, source_label, relationship, target_label, rows, *, on_batch=None,
                   require_endpoints=True, request_id=None):
        """Merge edges using the type's configured identity from bounded dictionaries.

        Each row contains ``source_id`` and ``target_id`` plus edge properties.
        Each batch is atomic; every endpoint must exist or that batch rolls back.
        ``require_endpoints=False`` instead skips rows whose endpoint is missing;
        the return value then counts only the edges actually merged.
        Omitted edge properties are preserved and supplied nulls remove properties.
        ``on_batch(batch_number, total_rows)`` runs after each successful commit.
        If that callback raises, the batch has already committed.
        Without ``request_id``, a full batch commits before the next row.

        ``request_id`` identifies one bounded batch on a supported server. All
        input is validated before a transaction opens; exceeding either batch
        bound raises without writing a prefix. Empty input sends no request.
        Requires quorum or opt-in authenticated standalone request-ledger mode.
        No retry occurs; use the appropriate outcome lookup after uncertainty,
        and never interpret ``not_recorded`` as permission to replay.
        """
        if on_batch is not None and not callable(on_batch):
            raise ValueError("on_batch must be callable")
        if request_id is not None:
            _request_id(request_id)
            if not require_endpoints:
                raise ValueError("identified edge batches require every endpoint to exist")
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
            operation = lambda tx: apply(tx, items)
            total += (self._write(operation) if request_id is None else
                      self.write_transaction(operation, request_id=request_id))
            batches += 1
            committed_rows += len(items)
            if on_batch is not None:
                on_batch(batches, committed_rows)

        for row in _input_rows(rows, self.batch_size):
            if (not isinstance(row, dict) or
                    (type(row) is dict and ("source_id" not in row or "target_id" not in row)) or
                    (type(row) is not dict and not {"source_id", "target_id"}.issubset(row))):
                raise ValueError("each edge row must contain source_id and target_id")
            if type(row) is dict and 5 * (len(row) - 2) > self.batch_bytes:
                raise BatchTooLargeError("edge", self.batch_bytes)
            budget = [self.batch_bytes, "edge", self.batch_bytes]
            props = _properties({key: value for key, value in row.items()
                                 if key not in ("source_id", "target_id")}, budget=budget)
            normalized = {"source_id": _entity(row["source_id"]),
                          "target_id": _entity(row["target_id"]), "properties": props}
            _charge_batch_value(budget, normalized["source_id"])
            _charge_batch_value(budget, normalized["target_id"])
            encoded = json.dumps(normalized, ensure_ascii=False, allow_nan=False,
                                 separators=(",", ":")).encode()
            if len(encoded) > self.batch_bytes:
                raise BatchTooLargeError("edge", self.batch_bytes)
            if batch and (len(batch) == self.batch_size or size + len(encoded) > self.batch_bytes):
                if request_id is not None:
                    raise ValueError("identified input must fit in one bounded batch")
                write_batch(batch)
                batch, size = [], 0
            batch.append(json.loads(encoded))
            size += len(encoded)
            if request_id is None and (len(batch) == self.batch_size or size == self.batch_bytes):
                write_batch(batch)
                batch, size = [], 0
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

    def _edge(self, source, relationship, target, properties, remove, require_existing,
              key=None, delete=False):
        if not isinstance(source, tuple) or not isinstance(target, tuple) or len(source) != 2 or len(target) != 2:
            raise ValueError("endpoints must be (label, entity_id) tuples")
        left, right, relationship = identifier(source[0]), identifier(target[0]), identifier(relationship)
        props = _properties({} if properties is None else properties, remove)
        if key is not None:
            if (not isinstance(key, dict) or not key
                    or any(not isinstance(value, str) or not value for value in key.values())):
                raise ValueError("key must be a nonempty dictionary of nonempty strings")
            key = _properties(key)
            if set(key) & set(props):
                raise ValueError("relationship key fields are immutable")
        assignments, values = _parameters(props)
        params = {"source": _entity(source[1]), "target": _entity(target[1]), **values}
        key_fields = ""
        if key is not None:
            key_fields = " {" + ", ".join(
                f"{name}:$key_{index}" for index, name in enumerate(sorted(key))) + "}"
            params.update({f"key_{index}": key[name] for index, name in enumerate(sorted(key))})
        match = f"MATCH (a:{left} {{entity_id:$source}}), (b:{right} {{entity_id:$target}})"
        fields = ", ".join(f"{name}:${parameter}" for name, parameter in assignments)
        edge = f"MERGE (a)-[:{relationship}" + (" {" + fields + "}" if fields else "") + "]->(b) RETURN count(*) AS processed"

        def apply(tx):
            if require_existing:
                if key is not None:
                    fields = _declared_relationship_key(tx, left, relationship, right)
                    if set(fields) != set(key):
                        raise ValueError("key must contain the complete declared relationship key")
                pattern = (f"MATCH (a:{left} {{entity_id:$source}})"
                           f"-[r:{relationship}{key_fields}]->(b:{right} {{entity_id:$target}})")
                query = pattern + " RETURN count(*) AS processed"
                count = tx.run(query, **params).single()["processed"]
                if count == 0:
                    return False
                if count != 1:
                    raise ValueError("edge mutation requires one merged relationship per selection")
                query = pattern
                if delete:
                    query += " DELETE r"
                elif assignments:
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

    def update_edge(self, source, relationship, target, properties=None, *, remove=(), key=None):
        """Patch the only existing relationship of a pair/type; never create one.

        For keyed types, pass every declared immutable field in ``key`` to
        select one timeline event. The catalog and mutation share a transaction.
        Omitting key retains the unique-pair behavior; ambiguous pairs are refused.
        """
        return self._edge(source, relationship, target, properties, remove, True, key=key)

    def delete_edge(self, source, relationship, target, *, key):
        """Delete one keyed relationship; return False when that event is absent.

        Supply every declared key field as a nonempty string. Catalog validation,
        selection and deletion share one transaction. Sibling events survive.
        Use ``delete_edges`` for deliberate endpoint-pair deletion.
        """
        if key is None:
            raise ValueError("key must be a nonempty dictionary of nonempty strings")
        return self._edge(source, relationship, target, {}, (), True, key=key, delete=True)

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
