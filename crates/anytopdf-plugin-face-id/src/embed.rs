//! ArcFace-style face embeddings through tract (pure-Rust ONNX inference).

use anyhow::{Context, Result, bail};
use image::RgbImage;
use sha2::{Digest, Sha256};
use std::path::Path;
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::Factoid;

type Runnable = std::sync::Arc<TypedRunnableModel>;

pub struct Embedder {
    model: Runnable,
    /// Square crop size the model expects.
    pub size: u32,
    nhwc: bool,
    /// First 16 hex digits of the model file's SHA-256.
    pub id: String,
}

impl Embedder {
    /// Loads an ONNX model whose single input is a 3-channel square crop, in
    /// NCHW (InsightFace ArcFace) or NHWC layout, with a static or dynamic
    /// batch dimension. The crop size comes from the model, else 112.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("read model {}", path.display()))?;
        let id = Sha256::digest(&bytes)
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        let model = tract_onnx::onnx()
            .model_for_read(&mut &bytes[..])
            .context("parse ONNX model")?;
        let dims: Vec<Option<usize>> = model
            .input_fact(0)
            .context("model has no input")?
            .shape
            .dims()
            .map(|d| {
                d.concretize()
                    .and_then(|d: TDim| d.as_i64())
                    .and_then(|d| usize::try_from(d).ok())
            })
            .collect();
        if dims.len() != 4 {
            bail!("model input must have 4 dimensions, found {}", dims.len());
        }
        let nhwc = dims[3] == Some(3) && dims[1] != Some(3);
        let size = if nhwc { dims[1] } else { dims[2] }.unwrap_or(112) as u32;
        if !(16..=1024).contains(&size) {
            bail!("unsupported model input size {size}");
        }
        let shape: [usize; 4] = if nhwc {
            [1, size as usize, size as usize, 3]
        } else {
            [1, 3, size as usize, size as usize]
        };
        let model = model
            .with_input_fact(0, f32::fact(shape).into())?
            .into_optimized()
            .context("optimize ONNX model")?
            .into_runnable()?;
        Ok(Self {
            model,
            size,
            nhwc,
            id,
        })
    }

    /// Embeds an aligned `size` x `size` RGB crop.
    pub fn embed(&self, crop: &RgbImage) -> Result<Vec<f32>> {
        if crop.dimensions() != (self.size, self.size) {
            bail!("crop must be {0}x{0}", self.size);
        }
        let n = self.size as usize;
        let value = |x: usize, y: usize, c: usize| {
            (f32::from(crop.get_pixel(x as u32, y as u32)[c]) - 127.5) / 127.5
        };
        let input: Tensor = if self.nhwc {
            tract_ndarray::Array4::from_shape_fn((1, n, n, 3), |(_, y, x, c)| value(x, y, c)).into()
        } else {
            tract_ndarray::Array4::from_shape_fn((1, 3, n, n), |(_, c, y, x)| value(x, y, c)).into()
        };
        let outputs = self.model.run(tvec!(input.into()))?;
        let output = outputs[0].to_plain_array_view::<f32>()?;
        let vector: Vec<f32> = output.iter().copied().collect();
        if vector.is_empty() || vector.iter().any(|v: &f32| !v.is_finite()) {
            bail!("model returned an empty or non-finite embedding");
        }
        Ok(anytopdf_faces::vector::normalize(vector))
    }
}
