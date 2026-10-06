//! Embedding vector helpers.

/// Scales `v` to unit length; a zero vector stays zero.
pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Cosine similarity; vectors of different lengths never match.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return -1.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        -1.0
    } else {
        dot / (na * nb)
    }
}

pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_ignores_scale_and_rejects_mismatched_lengths() {
        assert!((cosine(&[1.0, 0.0], &[5.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 2.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), -1.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), -1.0);
    }

    #[test]
    fn bytes_round_trip() {
        let v = normalize(vec![3.0, 4.0]);
        assert_eq!(v, vec![0.6, 0.8]);
        assert_eq!(from_bytes(&to_bytes(&v)), v);
    }
}
