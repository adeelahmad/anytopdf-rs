#!/usr/bin/env python3
"""Working protocol-v1 importer for UTF-8 .example files.

Copy into a trusted directory as anytopdf-plugin-example (or retain .py on Unix),
make executable, and add the directory to ANYTOPDF_PLUGIN_PATH.
"""
import argparse
import json
from pathlib import Path

PROTOCOL = 1


def manifest():
    print(json.dumps({
        "protocol": PROTOCOL,
        "name": "example-text",
        "version": "0.1.0",
        "capabilities": [{
            "kind": "importer", "extensions": ["example"],
            "mime_types": [], "priority": 50,
        }],
    }))


def request(request_path, response_path):
    response = {"protocol": PROTOCOL, "ok": False, "warnings": []}
    try:
        req = json.loads(Path(request_path).read_text(encoding="utf-8"))
        if req["protocol"] != PROTOCOL or req["operation"] != "import":
            raise ValueError("unsupported protocol or operation")
        source = req["source"]
        content = Path(source["path"]).read_text(encoding="utf-8")
        response.update(ok=True, units=[{
            "source_id": source["id"], "kind": "text", "visible_text": content,
            "annotations": [{"kind": "custom", "provider": "example-text", "text": "Imported UTF-8 example document"}],
        }])
    except (OSError, ValueError, KeyError) as error:
        response["error"] = str(error)
    destination = Path(response_path)
    temporary = destination.with_name(destination.name + ".tmp")
    temporary.write_text(json.dumps(response, ensure_ascii=False), encoding="utf-8")
    temporary.replace(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--anytopdf-manifest", action="store_true")
    parser.add_argument("--anytopdf-request")
    parser.add_argument("--anytopdf-response")
    args = parser.parse_args()
    if args.anytopdf_manifest:
        manifest()
    elif args.anytopdf_request and args.anytopdf_response:
        request(args.anytopdf_request, args.anytopdf_response)
    else:
        parser.error("provide a manifest flag or both request and response paths")


if __name__ == "__main__":
    main()
