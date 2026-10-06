//! Tiny in-memory ONNX models for tests. CI cannot download real weights, so
//! tests exercise the real loading and inference path with a graph whose
//! output is a fixed tensor.

use prost::Message;
use tract_onnx::pb::{
    AttributeProto, GraphProto, ModelProto, NodeProto, OperatorSetIdProto, StringStringEntryProto,
    TensorProto, TensorShapeProto, TypeProto, ValueInfoProto, tensor_shape_proto::Dimension,
    tensor_shape_proto::dimension, type_proto,
};

const FLOAT: i32 = 1;

fn value_info(name: &str, dims: &[Option<i64>]) -> ValueInfoProto {
    let dim = dims
        .iter()
        .enumerate()
        .map(|(i, d)| Dimension {
            value: Some(match d {
                Some(n) => dimension::Value::DimValue(*n),
                None => dimension::Value::DimParam(format!("d{i}")),
            }),
            ..Default::default()
        })
        .collect();
    ValueInfoProto {
        name: name.into(),
        r#type: Some(TypeProto {
            value: Some(type_proto::Value::TensorType(type_proto::Tensor {
                elem_type: FLOAT,
                shape: Some(TensorShapeProto { dim }),
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn tensor(name: &str, dims: &[i64], values: Vec<f32>) -> TensorProto {
    TensorProto {
        name: name.into(),
        dims: dims.to_vec(),
        data_type: FLOAT,
        float_data: values,
        ..Default::default()
    }
}

fn node(op: &str, inputs: &[&str], output: &str) -> NodeProto {
    NodeProto {
        op_type: op.into(),
        input: inputs.iter().map(|s| s.to_string()).collect(),
        output: vec![output.into()],
        name: output.into(),
        ..Default::default()
    }
}

/// A model with one `[1, 3, size, size]` input named `images` (dynamic height
/// and width when `size` is `None`) whose single output `output0` always
/// equals `values` with shape `output_shape`. The input still flows through
/// the graph (multiplied by zero), so the whole preprocessing path runs.
pub fn constant_model(
    size: Option<i64>,
    output_shape: &[i64],
    values: Vec<f32>,
    metadata: &[(&str, &str)],
) -> ModelProto {
    let graph = GraphProto {
        name: "fixture".into(),
        node: vec![
            NodeProto {
                attribute: vec![AttributeProto {
                    name: "keepdims".into(),
                    r#type: 2, // INT
                    i: 0,
                    ..Default::default()
                }],
                ..node("ReduceMean", &["images"], "mean")
            },
            node("Mul", &["mean", "zero"], "nothing"),
            node("Add", &["constant", "nothing"], "output0"),
        ],
        initializer: vec![
            tensor("zero", &[], vec![0.0]),
            tensor("constant", output_shape, values),
        ],
        input: vec![value_info("images", &[Some(1), Some(3), size, size])],
        output: vec![value_info(
            "output0",
            &output_shape.iter().map(|d| Some(*d)).collect::<Vec<_>>(),
        )],
        ..Default::default()
    };
    ModelProto {
        ir_version: 8,
        opset_import: vec![OperatorSetIdProto {
            domain: String::new(),
            version: 13,
        }],
        producer_name: "anytopdf-onnx-testing".into(),
        graph: Some(graph),
        metadata_props: metadata
            .iter()
            .map(|(k, v)| StringStringEntryProto {
                key: k.to_string(),
                value: v.to_string(),
            })
            .collect(),
        ..Default::default()
    }
}

/// Serializes a model to `.onnx` bytes.
pub fn encode(model: &ModelProto) -> Vec<u8> {
    model.encode_to_vec()
}
