#!/usr/bin/env python3
"""Serve Moondream2 locally over the Moondream API for anytopdf-plugin-vlm.

This is the Python backend from the original video-analysis script: it loads
vikhyatk/moondream2 with transformers once and answers

  POST /v1/caption {"image_url": "data:image/jpeg;base64,...", "length": "normal"} -> {"caption": ...}
  POST /v1/query   {"image_url": ..., "question": "..."}                           -> {"answer": ...}
  POST /v1/detect  {"image_url": ..., "object": "face"}                            -> {"objects": [...]}
  GET  /v1/health                                                                  -> {"status": "ok"}

Run it, then point the plugin at it:

  python3 -m pip install "transformers>=4.51" torch pillow
  python3 helpers/anytopdf-moondream-server.py --device cpu
  export ANYTOPDF_VLM_URL=http://127.0.0.1:2020/v1 ANYTOPDF_VLM_ENGINE=moondream

It listens on 127.0.0.1 only and handles one request at a time.
"""
import argparse
import base64
import binascii
import io
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

MAX_BODY = 32 * 1024 * 1024
DEFAULT_PORT = 2020


def load_model(model_id, revision, device):
    """Loads Moondream2 the way the original script did."""
    import torch  # noqa: F401  (transformers needs it; imported for a clear error)
    from transformers import AutoModelForCausalLM
    from transformers.generation.utils import GenerationMixin

    # HfMoondream is remote code; skip the optional custom_generate hook that
    # some transformers releases try to load for it.
    original = GenerationMixin.load_custom_generate

    def skip(*_args, **_kwargs):
        raise OSError("Skipping optional custom_generate hook for Moondream2.")

    GenerationMixin.load_custom_generate = skip
    try:
        model = AutoModelForCausalLM.from_pretrained(model_id, revision=revision, trust_remote_code=True)
    finally:
        GenerationMixin.load_custom_generate = original
    if device == "mps":
        # Moondream2 mixes CPU and MPS tensors in its vision encoder.
        print("moondream: MPS is not supported by Moondream2; using CPU", file=sys.stderr)
        device = "cpu"
    if device != "cpu":
        model = model.to(device)
    model.eval()
    return model


def decode_image(image_url):
    from PIL import Image

    if not isinstance(image_url, str) or not image_url.startswith("data:image/"):
        raise ValueError("image_url must be a data:image/...;base64 URL")
    _, _, data = image_url.partition(",")
    try:
        raw = base64.b64decode(data, validate=True)
    except binascii.Error as error:
        raise ValueError(f"image_url is not valid base64: {error}") from error
    return Image.open(io.BytesIO(raw)).convert("RGB")


def handle(model, decode, path, body):
    """Returns (status, JSON object) for one API request."""
    if path == "/v1/health":
        return 200, {"status": "ok"}
    if path not in ("/v1/caption", "/v1/query", "/v1/detect"):
        return 404, {"error": f"unknown endpoint {path}"}
    if not isinstance(body, dict):
        return 400, {"error": "request body must be a JSON object"}
    try:
        image = decode(body.get("image_url"))
    except Exception as error:  # noqa: BLE001  (any decode failure is the client's)
        return 400, {"error": f"cannot read image: {error}"}
    if path == "/v1/caption":
        length = body.get("length", "normal")
        if length not in ("short", "normal", "long"):
            return 400, {"error": "length must be short, normal or long"}
        return 200, {"caption": str(model.caption(image, length=length)["caption"]).strip()}
    if path == "/v1/query":
        question = body.get("question")
        if not isinstance(question, str) or not question.strip():
            return 400, {"error": "question must be a non-empty string"}
        return 200, {"answer": str(model.query(image, question)["answer"]).strip()}
    target = body.get("object")
    if not isinstance(target, str) or not target.strip():
        return 400, {"error": "object must be a non-empty string"}
    objects = []
    for found in model.detect(image, target).get("objects", []):
        try:
            box = {k: float(found[k]) for k in ("x_min", "y_min", "x_max", "y_max")}
        except (KeyError, TypeError, ValueError):
            continue
        objects.append(box)
    return 200, {"objects": objects}


def make_handler(model, decode):
    class Handler(BaseHTTPRequestHandler):
        server_version = "anytopdf-moondream"

        def reply(self, status, payload):
            data = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):  # noqa: N802
            self.reply(*handle(model, decode, self.path, {}))

        def do_POST(self):  # noqa: N802
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                length = -1
            if not 0 <= length <= MAX_BODY:
                self.reply(413, {"error": "request body too large"})
                return
            try:
                body = json.loads(self.rfile.read(length) or b"null")
            except (ValueError, UnicodeDecodeError):
                self.reply(400, {"error": "request body is not JSON"})
                return
            try:
                self.reply(*handle(model, decode, self.path, body))
            except Exception as error:  # noqa: BLE001  (report model failures to the plugin)
                self.reply(500, {"error": f"model failed: {error}"})

        def log_message(self, fmt, *args):
            print(f"moondream: {fmt % args}", file=sys.stderr)

    return Handler


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--device", default="cpu", help="cpu, cuda, cuda:N or mps (falls back to cpu)")
    parser.add_argument("--model", default="vikhyatk/moondream2")
    parser.add_argument("--revision", default=None, help="model revision to pin, for example a release date")
    args = parser.parse_args(argv)
    model = load_model(args.model, args.revision, args.device)
    server = HTTPServer(("127.0.0.1", args.port), make_handler(model, decode_image))
    print(f"moondream: serving on http://127.0.0.1:{server.server_port}/v1", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
