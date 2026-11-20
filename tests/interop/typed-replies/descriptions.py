import json
from pathlib import Path
import sys


def resolve(doc, schema):
    node = doc
    for key in schema["$ref"].removeprefix("#/").split("/"):
        node = node[key]
    return node


def check(op, doc, schema, success, errors):
    assert op["x-csil-success-variant"] == success
    assert schema["$ref"].endswith("/" + success)
    assert resolve(doc, schema)
    assert set(op["x-csil-error-variants"]) == set(errors)
    for name, error in op["x-csil-error-variants"].items():
        assert error["$ref"].endswith("/" + name)
        assert resolve(doc, error)


target = sys.argv[1]
if target == "json":
    doc = json.loads(Path("api-service.json").read_text())

    def operation(name):
        op = doc["properties"][name]
        return op, op["properties"]["output"]
else:
    doc = json.loads(Path("openapi.json").read_text())

    def operation(name):
        op = doc["paths"]["/api/" + name]["post"]["responses"]["200"]
        return op, op["content"]["application/json"]["schema"]

definitions = doc.get("$defs", doc.get("definitions", doc.get("components", {}).get("schemas", {})))
assert definitions["RefusalCode"]["enum"] == ["busy", "denied"]
assert definitions["RefusalCode"]["x-csil-cbor-values"] == {"busy": 7, "denied": 42}

for name, success, errors in [("submit", "Resp", ["Limit", "Retry"]), ("poll", "Reply", ["Retry"])]:
    op, schema = operation(name)
    check(op, doc, schema, success, errors)

op, schema = operation("fetch")
assert "x-csil-error-variants" not in op
assert schema["$ref"].endswith("/Reply")
assert resolve(doc, schema)
