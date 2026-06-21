#![forbid(unsafe_code)]

//! Safe-Rust compatibility implementation for the subset of SimSIMD used by
//! Turso's vector SQL functions. Herdcore does not use those functions, but
//! providing correct scalar implementations avoids an otherwise unconditional
//! C compilation step in the upstream dependency.

pub type Distance = f64;

pub trait SpatialSimilarity
where
    Self: Sized,
{
    fn cos(a: &[Self], b: &[Self]) -> Option<Distance>;
    fn dot(a: &[Self], b: &[Self]) -> Option<Distance>;
    fn l2sq(a: &[Self], b: &[Self]) -> Option<Distance>;
    fn l2(a: &[Self], b: &[Self]) -> Option<Distance>;

    fn sqeuclidean(a: &[Self], b: &[Self]) -> Option<Distance> {
        Self::l2sq(a, b)
    }

    fn euclidean(a: &[Self], b: &[Self]) -> Option<Distance> {
        Self::l2(a, b)
    }

    fn inner(a: &[Self], b: &[Self]) -> Option<Distance> {
        Self::dot(a, b)
    }

    fn cosine(a: &[Self], b: &[Self]) -> Option<Distance> {
        Self::cos(a, b)
    }
}

macro_rules! impl_spatial_similarity {
    ($number:ty) => {
        impl SpatialSimilarity for $number {
            fn cos(a: &[Self], b: &[Self]) -> Option<Distance> {
                if a.len() != b.len() {
                    return None;
                }
                let (dot, norm_a, norm_b) = a.iter().zip(b).fold(
                    (0.0_f64, 0.0_f64, 0.0_f64),
                    |(dot, norm_a, norm_b), (&a, &b)| {
                        let a = a as f64;
                        let b = b as f64;
                        (dot + a * b, norm_a + a * a, norm_b + b * b)
                    },
                );
                if norm_a == 0.0 || norm_b == 0.0 {
                    Some(0.0)
                } else {
                    Some(1.0 - dot / (norm_a * norm_b).sqrt())
                }
            }

            fn dot(a: &[Self], b: &[Self]) -> Option<Distance> {
                (a.len() == b.len()).then(|| {
                    a.iter()
                        .zip(b)
                        .map(|(&a, &b)| (a as f64) * (b as f64))
                        .sum()
                })
            }

            fn l2sq(a: &[Self], b: &[Self]) -> Option<Distance> {
                (a.len() == b.len()).then(|| {
                    a.iter()
                        .zip(b)
                        .map(|(&a, &b)| {
                            let delta = (a as f64) - (b as f64);
                            delta * delta
                        })
                        .sum()
                })
            }

            fn l2(a: &[Self], b: &[Self]) -> Option<Distance> {
                Self::l2sq(a, b).map(f64::sqrt)
            }
        }
    };
}

impl_spatial_similarity!(f32);
impl_spatial_similarity!(f64);

#[cfg(test)]
mod tests {
    use super::SpatialSimilarity;

    #[test]
    fn scalar_metrics_match_expected_values() {
        assert_eq!(f32::dot(&[1.0, 2.0], &[3.0, 4.0]), Some(11.0));
        assert_eq!(f64::euclidean(&[0.0, 0.0], &[3.0, 4.0]), Some(5.0));
        assert_eq!(f32::cosine(&[1.0, 0.0], &[0.0, 1.0]), Some(1.0));
        assert_eq!(f32::dot(&[1.0], &[]), None);
    }
}
