"""Export an explicit DuckLake snapshot from a local SQLite catalog.

Requires cached ducklake/sqlite extensions; this example never INSTALLs them.
Only local catalog files and managed data directories are supported. Keep the
catalog and managed files available for the pinned snapshot throughout export;
external file edits or snapshot expiration are outside this consistency contract.
The query is trusted SQL. This snapshot bridge does not implement graph CDC.
"""

import argparse
import json
from pathlib import Path

from examples.client.export_snapshot import export_snapshot


def sql_literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def export_ducklake_snapshot(catalog, data_path, snapshot_version, query, destination,
                            fetch_rows=250, line_bytes=65536, max_output_bytes=1 << 30):
    """Read source_lake.main at one version and return the export receipt.

The absolute DATA_PATH spelling must match the catalog; symlinks are validated
but preserved for ATTACH, and automatic data-path overrides are disabled.
"""
    if type(snapshot_version) is not int or snapshot_version < 0:
        raise ValueError("snapshot-version must be a nonnegative integer")
    catalog = Path(catalog).resolve(strict=True)
    data_path = Path(data_path).absolute()
    resolved_data_path = data_path.resolve(strict=True)
    if not catalog.is_file():
        raise ValueError("catalog must be an existing local SQLite file")
    if not resolved_data_path.is_dir():
        raise ValueError("data-path must be an existing local directory")
    import duckdb

    with duckdb.connect(config={"autoinstall_known_extensions": "false",
                                "autoload_known_extensions": "false"}) as connection:
        connection.execute("LOAD ducklake")
        connection.execute("LOAD sqlite")
        version = connection.execute("SELECT extension_version FROM duckdb_extensions() WHERE extension_name = 'ducklake'").fetchone()[0]
        connection.execute(
            f"ATTACH {sql_literal('ducklake:sqlite:' + str(catalog))} AS source_lake "
            f"(DATA_PATH {sql_literal(str(data_path) + '/')}, READ_ONLY, OVERRIDE_DATA_PATH false, SNAPSHOT_VERSION {snapshot_version})"
        )
        receipt = export_snapshot(connection, query, destination, fetch_rows, line_bytes, max_output_bytes)
    receipt.update(source_type="ducklake-sqlite", snapshot_version=snapshot_version,
                   duckdb_version=duckdb.__version__, ducklake_extension_version=version)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", required=True)
    parser.add_argument("--data-path", required=True)
    parser.add_argument("--snapshot-version", required=True, type=int)
    parser.add_argument("--query-file", required=True, help="trusted SELECT against source_lake.main")
    parser.add_argument("--output", required=True)
    parser.add_argument("--fetch-rows", type=int, default=250)
    parser.add_argument("--line-bytes", type=int, default=65536)
    parser.add_argument("--max-output-bytes", type=int, default=1 << 30)
    args = parser.parse_args()
    receipt = export_ducklake_snapshot(args.catalog, args.data_path, args.snapshot_version,
                                      Path(args.query_file).read_text(encoding="utf-8"), args.output,
                                      args.fetch_rows, args.line_bytes, args.max_output_bytes)
    print(json.dumps(receipt, sort_keys=True))


if __name__ == "__main__":
    main()
