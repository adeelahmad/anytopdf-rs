"""Regenerates tiny-embed-raw.onnx, a stand-in for embedding models that scale
their own input (OpenCV SFace, ONNX-zoo ArcFace): it starts with the same
`Sub 127.5`, `Mul 1/127.5` nodes, so it expects raw 0-255 pixels.

It then averages each channel over the four quadrants of the 1x3x112x112 crop,
giving a 12-value embedding. Requires the `onnx` package; run from this
directory.
"""

import onnx
from onnx import TensorProto, helper

graph = helper.make_graph(
    [
        helper.make_node("Sub", ["data", "mean"], ["centered"]),
        helper.make_node("Mul", ["centered", "scale"], ["scaled"]),
        helper.make_node(
            "AveragePool", ["scaled"], ["pooled"], kernel_shape=[56, 56], strides=[56, 56]
        ),
        helper.make_node("Flatten", ["pooled"], ["embedding"], axis=1),
    ],
    "tiny-embed-raw",
    [helper.make_tensor_value_info("data", TensorProto.FLOAT, [1, 3, 112, 112])],
    [helper.make_tensor_value_info("embedding", TensorProto.FLOAT, [1, 12])],
    [
        helper.make_tensor("mean", TensorProto.FLOAT, [], [127.5]),
        helper.make_tensor("scale", TensorProto.FLOAT, [], [1 / 127.5]),
    ],
)
model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 13)])
model.ir_version = 8
onnx.checker.check_model(model)
onnx.save(model, "tiny-embed-raw.onnx")
