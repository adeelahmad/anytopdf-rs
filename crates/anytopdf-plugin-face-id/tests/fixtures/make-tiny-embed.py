"""Regenerates tiny-embed.onnx, a stand-in face embedding model for tests.

It averages each RGB channel of a 1x3x112x112 crop and projects the three
means onto four outputs, so different crops give different, deterministic
embeddings. Requires the `onnx` package; run from this directory.
"""

import onnx
from onnx import TensorProto, helper

weights = helper.make_tensor(
    "w", TensorProto.FLOAT, [3, 4], [1, 0, 0, 1, 0, 1, 0, -1, 0, 0, 1, 0.5]
)
graph = helper.make_graph(
    [
        helper.make_node("GlobalAveragePool", ["input"], ["pooled"]),
        helper.make_node("Flatten", ["pooled"], ["flat"], axis=1),
        helper.make_node("MatMul", ["flat", "w"], ["embedding"]),
    ],
    "tiny-embed",
    [helper.make_tensor_value_info("input", TensorProto.FLOAT, [1, 3, 112, 112])],
    [helper.make_tensor_value_info("embedding", TensorProto.FLOAT, [1, 4])],
    [weights],
)
model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 13)])
model.ir_version = 8
onnx.checker.check_model(model)
onnx.save(model, "tiny-embed.onnx")
