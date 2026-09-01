import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest


ADAPTER_PATH = Path(__file__).parents[1] / "scripts" / "diaphora_adapter.py"
SPEC = importlib.util.spec_from_file_location("diaphora_adapter", ADAPTER_PATH)
ADAPTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ADAPTER)


def create_export(path, rows):
    connection = sqlite3.connect(path)
    connection.execute(
        "CREATE TABLE functions "
        "(address TEXT, name TEXT, prototype TEXT, pseudocode TEXT)"
    )
    connection.executemany("INSERT INTO functions VALUES (?, ?, ?, ?)", rows)
    connection.commit()
    connection.close()


class AdapterResultsTest(unittest.TestCase):
    def test_collects_matches_and_unmatched_functions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = root / "old.sqlite"
            new = root / "new.sqlite"
            results = root / "results.sqlite"
            create_export(
                old,
                [
                    (str(0x401000), "parse", "int parse(void)", "{ return 1; }"),
                    (str(0x402000), "removed", "void removed(void)", "{}"),
                ],
            )
            create_export(
                new,
                [
                    (str(0x501000), "parse", "int parse(void)", "{ return 2; }"),
                    (str(0x503000), "added", "void added(void)", "{}"),
                ],
            )
            connection = sqlite3.connect(results)
            connection.execute(
                "CREATE TABLE results "
                "(type TEXT, address TEXT, address2 TEXT, ratio REAL, description TEXT)"
            )
            connection.execute("CREATE TABLE unmatched (type TEXT, address TEXT)")
            connection.execute(
                "INSERT INTO results VALUES (?, ?, ?, ?, ?)",
                ("partial", "00401000", "00501000", 0.88, "pseudo-code match"),
            )
            connection.executemany(
                "INSERT INTO unmatched VALUES (?, ?)",
                [("primary", "00402000"), ("secondary", "00503000")],
            )
            connection.commit()
            connection.close()

            response = ADAPTER.collect_response(old, new, results)

        self.assertEqual(response["protocol_version"], 1)
        by_status = {item["status"]: item for item in response["functions"]}
        self.assertEqual(by_status["modified"]["similarity"], 0.88)
        self.assertEqual(by_status["modified"]["match_category"], "partial")
        self.assertIn("int parse(void)", by_status["modified"]["old_pseudocode"])
        self.assertIn("added", by_status)
        self.assertIn("deleted", by_status)

    def test_marks_unreliable_matches_unresolved(self):
        function = {"address": 1, "name": "f", "pseudocode": "void f() {}"}
        record = ADAPTER._record("unreliable", function, function, 0.7, "weak")
        self.assertEqual(record["status"], "unresolved")


if __name__ == "__main__":
    unittest.main()
