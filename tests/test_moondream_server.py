import importlib.util
import json
from http.server import HTTPServer
from pathlib import Path
import threading
import unittest
import urllib.error
import urllib.request

SERVER = Path(__file__).resolve().parents[1] / "helpers/anytopdf-moondream-server.py"
spec = importlib.util.spec_from_file_location("moondream_server", SERVER)
moondream = importlib.util.module_from_spec(spec)
spec.loader.exec_module(moondream)


class StubModel:
    """Stands in for Moondream2, which CI cannot download."""

    def caption(self, image, length):
        return {"caption": f" a {image} ({length}) "}

    def query(self, image, question):
        return {"answer": f"{question} -> {image}"}

    def detect(self, image, target):
        return {"objects": [{"x_min": 0.1, "y_min": 0.2, "x_max": 0.5, "y_max": 0.6}, {"x_min": "bad"}]}


def decode(image_url):
    if image_url != "data:image/jpeg;base64,AAAA":
        raise ValueError("not an image")
    return "frame"


class HandleTests(unittest.TestCase):
    def call(self, path, body):
        return moondream.handle(StubModel(), decode, path, body)

    def test_caption_query_and_detect(self):
        image = {"image_url": "data:image/jpeg;base64,AAAA"}
        self.assertEqual(self.call("/v1/caption", image), (200, {"caption": "a frame (normal)"}))
        self.assertEqual(
            self.call("/v1/query", {**image, "question": "Who?"}), (200, {"answer": "Who? -> frame"})
        )
        status, body = self.call("/v1/detect", {**image, "object": "face"})
        self.assertEqual(status, 200)
        self.assertEqual(body["objects"], [{"x_min": 0.1, "y_min": 0.2, "x_max": 0.5, "y_max": 0.6}])

    def test_bad_requests_are_client_errors(self):
        image = {"image_url": "data:image/jpeg;base64,AAAA"}
        self.assertEqual(self.call("/v1/caption", {"image_url": "file:///etc/passwd"})[0], 400)
        self.assertEqual(self.call("/v1/caption", {**image, "length": "huge"})[0], 400)
        self.assertEqual(self.call("/v1/query", image)[0], 400)
        self.assertEqual(self.call("/v1/detect", {**image, "object": " "})[0], 400)
        self.assertEqual(self.call("/v1/caption", [])[0], 400)
        self.assertEqual(self.call("/v1/chat/completions", image)[0], 404)
        self.assertEqual(self.call("/v1/health", {}), (200, {"status": "ok"}))

    def test_real_decoder_rejects_non_data_urls(self):
        try:
            import PIL  # noqa: F401
        except ImportError:
            self.skipTest("Pillow is not installed")
        with self.assertRaises(ValueError):
            moondream.decode_image("https://example.com/a.jpg")
        with self.assertRaises(ValueError):
            moondream.decode_image("data:image/jpeg;base64,!!")


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.server = HTTPServer(("127.0.0.1", 0), moondream.make_handler(StubModel(), decode))
        self.server.RequestHandlerClass.log_message = lambda *args: None
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = f"http://127.0.0.1:{self.server.server_port}/v1"

    def post(self, path, payload):
        request = urllib.request.Request(
            f"{self.base}/{path}", data=payload, headers={"Content-Type": "application/json"}, method="POST"
        )
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=10) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def test_round_trip_over_http(self):
        body = json.dumps({"image_url": "data:image/jpeg;base64,AAAA", "question": "What?"}).encode()
        self.assertEqual(self.post("query", body), (200, {"answer": "What? -> frame"}))
        self.assertEqual(self.post("query", b"{not json")[0], 400)


if __name__ == "__main__":
    unittest.main()
