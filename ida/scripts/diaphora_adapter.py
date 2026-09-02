"""Narrow IDA/Diaphora adapter for diffplus protocol version 1.

IDA runs the ``export`` operation. A normal Python interpreter runs the
``compare`` operation after both exports exist. Keeping SQLite translation in
this file prevents IDAPython and Diaphora internals leaking into the Rust core.
"""

import hashlib
import importlib
import json
import os
from pathlib import Path
import runpy
import sqlite3
import sys


PROTOCOL_VERSION = 1
AUTOMATIC_NAME_PREFIXES = ("sub_", "loc_", "nullsub_", "j_")


def _load_request():
    request_path = os.environ.get("DIFFPLUS_REQUEST")
    if not request_path:
        raise RuntimeError("DIFFPLUS_REQUEST is not set")
    with open(request_path, "r", encoding="utf-8") as handle:
        request = json.load(handle)
    if request.get("protocol_version") != PROTOCOL_VERSION:
        raise RuntimeError("unsupported request protocol version")
    return request


def _configure_diaphora(path):
    root = Path(path).resolve()
    if not (root / "diaphora.py").is_file():
        raise RuntimeError(f"diaphora.py not found in {root}")
    if str(root) not in sys.path:
        sys.path.insert(0, str(root))
    return root


def export_database(request):
    """Delegate an IDA batch export to Diaphora's supported automation path."""
    root = _configure_diaphora(request["diaphora_path"])
    os.environ["DIAPHORA_AUTO"] = "1"
    os.environ["DIAPHORA_EXPORT_FILE"] = request["export_database"]
    os.environ["DIAPHORA_USE_DECOMPILER"] = "1"
    runpy.run_path(str(root / "diaphora_ida.py"), run_name="__main__")


def compare_databases(request):
    """Run Diaphora's public database diff flow, then emit stable JSON."""
    _configure_diaphora(request["diaphora_path"])
    diaphora = importlib.import_module("diaphora")
    engine = diaphora.CBinDiff(request["old_database"])
    try:
        if not engine.diff(request["new_database"]):
            raise RuntimeError("Diaphora did not complete the database comparison")
        engine.save_results(request["results_database"])
    finally:
        try:
            engine.db_close()
        except Exception:
            pass

    response = collect_response(
        request["old_database"],
        request["new_database"],
        request["results_database"],
    )
    with open(request["output"], "w", encoding="utf-8") as handle:
        json.dump(response, handle, indent=2, sort_keys=True)


def _connect(path):
    connection = sqlite3.connect(path)
    connection.row_factory = sqlite3.Row
    return connection


def _database_address(result_address):
    """Diaphora results use hexadecimal text; exports use decimal text."""
    if result_address is None:
        return None
    return str(int(str(result_address), 16))


def _function(connection, result_address):
    if result_address is None:
        return None
    row = connection.execute(
        "SELECT address, name, prototype, pseudocode FROM functions WHERE address = ?",
        (_database_address(result_address),),
    ).fetchone()
    if row is None:
        return None
    pseudocode = row["pseudocode"]
    prototype = row["prototype"]
    if pseudocode and prototype:
        pseudocode = prototype.rstrip() + "\n" + pseudocode.lstrip()
    return {
        "address": int(row["address"]),
        "name": row["name"],
        "pseudocode": pseudocode,
    }


def _readable_name(old_function, new_function):
    names = [
        function["name"]
        for function in (old_function, new_function)
        if function and function.get("name")
    ]
    for name in names:
        if not name.lower().startswith(AUTOMATIC_NAME_PREFIXES):
            return name
    return names[0] if names else "function"


def _stable_id(category, old_function, new_function):
    name = _readable_name(old_function, new_function)
    readable = "".join(char if char.isalnum() or char in "_-" else "_" for char in name)
    identity = "\0".join(
        (
            category,
            str(old_function["address"] if old_function else ""),
            str(new_function["address"] if new_function else ""),
        )
    )
    suffix = hashlib.sha256(identity.encode("utf-8")).hexdigest()[:12]
    return f"{readable or 'function'}_{suffix}"


def _record(category, old_function, new_function, ratio=None, reason=None):
    old_pseudo = old_function.get("pseudocode") if old_function else None
    new_pseudo = new_function.get("pseudocode") if new_function else None
    if category in ("unreliable", "multimatch") or not old_pseudo and not new_pseudo:
        status = "unresolved"
    elif old_function is None:
        status = "added" if new_pseudo else "unresolved"
    elif new_function is None:
        status = "deleted" if old_pseudo else "unresolved"
    elif not old_pseudo or not new_pseudo:
        status = "unresolved"
    elif old_pseudo == new_pseudo:
        status = "unchanged"
    else:
        status = "modified"
    return {
        "stable_id": _stable_id(category, old_function, new_function),
        "old_address": old_function.get("address") if old_function else None,
        "new_address": new_function.get("address") if new_function else None,
        "old_name": old_function.get("name") if old_function else None,
        "new_name": new_function.get("name") if new_function else None,
        "status": status,
        "similarity": float(ratio) if ratio is not None else None,
        "match_category": category,
        "match_reason": reason,
        "old_pseudocode": old_pseudo,
        "new_pseudocode": new_pseudo,
    }


def collect_response(old_database, new_database, results_database):
    """Translate Diaphora SQLite output into the versioned wire model."""
    old = _connect(old_database)
    new = _connect(new_database)
    results = _connect(results_database)
    functions = []
    try:
        rows = results.execute(
            "SELECT type, address, address2, ratio, description FROM results "
            "ORDER BY type, address, address2"
        )
        for row in rows:
            functions.append(
                _record(
                    row["type"],
                    _function(old, row["address"]),
                    _function(new, row["address2"]),
                    row["ratio"],
                    row["description"],
                )
            )

        rows = results.execute(
            "SELECT type, address FROM unmatched ORDER BY type, address"
        )
        for row in rows:
            if row["type"] == "primary":
                functions.append(_record("unmatched_primary", _function(old, row["address"]), None))
            elif row["type"] == "secondary":
                functions.append(_record("unmatched_secondary", None, _function(new, row["address"])))
            else:
                raise RuntimeError(f"unknown Diaphora unmatched category: {row['type']}")
    finally:
        old.close()
        new.close()
        results.close()
    return {"protocol_version": PROTOCOL_VERSION, "functions": functions}


def main():
    request = _load_request()
    operation = request.get("operation")
    if operation == "export":
        export_database(request)
    elif operation == "compare":
        compare_databases(request)
    else:
        raise RuntimeError(f"unsupported adapter operation: {operation}")


if __name__ == "__main__":
    main()
