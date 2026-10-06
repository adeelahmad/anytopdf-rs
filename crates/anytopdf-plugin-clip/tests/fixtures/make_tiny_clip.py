"""Regenerate the tiny CLIP fixture used by the plugin tests.

    python3 -m pip install onnx
    python3 crates/anytopdf-plugin-clip/tests/fixtures/make_tiny_clip.py

The image encoder averages each normalized colour channel, so red, green and
blue images embed near the x, y and z axes. The text encoder sums a 3-d vector
per token: "red", "green" and "blue" point along the same axes and every other
token is zero. Input and output names follow the real ViT-B/32 export.
"""
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

OUT = Path(__file__).resolve().parent / "tiny-clip"
MERGES = ["r e", "re d</w>", "b l", "bl u", "blu e</w>", "g r", "gr e", "gre e", "gree n</w>"]
# OpenAI layout: 256 byte tokens, 256 word-end byte tokens, merges, markers.
VOCAB = 512 + len(MERGES) + 2
RED, BLUE, GREEN = 512 + 1, 512 + 4, 512 + 8


def save(graph, name):
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 14)])
    model.ir_version = 7
    onnx.checker.check_model(model)
    onnx.save(model, OUT / name)


def visual():
    pixels = helper.make_tensor_value_info("pixel_values", TensorProto.FLOAT, ["batch_size", 3, 224, 224])
    output = helper.make_tensor_value_info("output", TensorProto.FLOAT, ["batch", 3])
    mean = helper.make_node("ReduceMean", ["pixel_values"], ["output"], axes=[2, 3], keepdims=0)
    save(helper.make_graph([mean], "tiny-visual", [pixels], [output]), "visual.onnx")


def textual():
    table = np.zeros((VOCAB, 3), dtype=np.float32)
    table[RED] = [1, 0, 0]
    table[GREEN] = [0, 1, 0]
    table[BLUE] = [0, 0, 1]
    ids = helper.make_tensor_value_info("input_ids", TensorProto.INT32, ["batch_size", "sequence"])
    mask = helper.make_tensor_value_info("attention_mask", TensorProto.INT32, ["batch_size", "sequence"])
    output = helper.make_tensor_value_info("output", TensorProto.FLOAT, ["batch", 3])
    nodes = [
        helper.make_node("Gather", ["table", "input_ids"], ["vectors"], axis=0),
        helper.make_node("Cast", ["attention_mask"], ["mask_f"], to=TensorProto.FLOAT),
        helper.make_node("Unsqueeze", ["mask_f", "last"], ["mask_3d"]),
        helper.make_node("Mul", ["vectors", "mask_3d"], ["masked"]),
        helper.make_node("ReduceSum", ["masked", "seq_axis"], ["output"], keepdims=0),
    ]
    init = [
        numpy_helper.from_array(table, "table"),
        numpy_helper.from_array(np.array([2], dtype=np.int64), "last"),
        numpy_helper.from_array(np.array([1], dtype=np.int64), "seq_axis"),
    ]
    save(helper.make_graph(nodes, "tiny-textual", [ids, mask], [output], init), "textual.onnx")


def merges():
    (OUT / "bpe_simple_vocab_16e6.txt").write_text("#version: 0.2\n" + "\n".join(MERGES) + "\n")


if __name__ == "__main__":
    OUT.mkdir(exist_ok=True)
    visual()
    textual()
    merges()
