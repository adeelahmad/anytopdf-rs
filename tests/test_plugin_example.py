import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import uuid

PLUGIN = Path(__file__).resolve().parents[1] / "examples/anytopdf-plugin-example.py"


class PluginExampleTests(unittest.TestCase):
    def test_manifest(self):
        result = subprocess.run([sys.executable, str(PLUGIN), "--anytopdf-manifest"], check=True, capture_output=True)
        manifest = json.loads(result.stdout)
        self.assertEqual(manifest["protocol"], 1)
        self.assertEqual(manifest["capabilities"][0]["extensions"], ["example"])

    def test_import_and_error_response(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "document.example"
            source.write_text("Hello café", encoding="utf-8")
            request = root / "request.json"
            response = root / "response.json"
            source_id = str(uuid.uuid4())
            request.write_text(json.dumps({"protocol": 1, "operation": "import", "source": {"id": source_id, "path": str(source)}}))
            command = [sys.executable, str(PLUGIN), "--anytopdf-request", str(request), "--anytopdf-response", str(response)]
            subprocess.run(command, check=True)
            result = json.loads(response.read_text(encoding="utf-8"))
            self.assertTrue(result["ok"])
            self.assertEqual(result["units"][0]["visible_text"], "Hello café")
            self.assertEqual(result["units"][0]["source_id"], source_id)
            source.unlink()
            subprocess.run(command, check=True)
            self.assertFalse(json.loads(response.read_text())["ok"])
            self.assertFalse((root / "response.json.tmp").exists())


if __name__ == "__main__":
    unittest.main()
