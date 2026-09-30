//! Compact sparse adjacency matrices for graph algorithms.
//!
//! The persistent graph remains the source of truth. This CSR projection is
//! intentionally small and rebuildable, so analytics can use matrix math
//! without turning a mostly-empty graph into a dense allocation.

#[derive(Debug, Clone, PartialEq)]
pub struct SparseMatrix {
    rows: usize,
    cols: usize,
    row_offsets: Vec<usize>,
    column_indices: Vec<usize>,
    values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageRankResult {
    pub scores: Vec<f64>,
    pub iterations: usize,
    pub converged: bool,
    /// L1 distance between the final two iterations.
    pub residual: f64,
}

impl SparseMatrix {
    /// Builds a CSR matrix from `(row, column, value)` entries.
    /// Duplicate coordinates are summed, which is useful when several graph
    /// events contribute weight to the same pair of nodes.
    pub fn from_entries(
        rows: usize,
        cols: usize,
        entries: impl IntoIterator<Item = (usize, usize, f32)>,
    ) -> Result<Self, SparseMatrixError> {
        let offset_count = rows.checked_add(1).ok_or(SparseMatrixError::Capacity)?;
        if offset_count > isize::MAX as usize / std::mem::size_of::<usize>() {
            return Err(SparseMatrixError::Capacity);
        }
        let mut entries: Vec<_> = entries.into_iter().collect();
        for &(row, col, value) in &entries {
            if row >= rows || col >= cols {
                return Err(SparseMatrixError::IndexOutOfBounds { row, col });
            }
            if !value.is_finite() {
                return Err(SparseMatrixError::NonFiniteValue);
            }
        }
        entries.sort_by_key(|&(row, col, _)| (row, col));
        let mut compact: Vec<(usize, usize, f32)> = Vec::with_capacity(entries.len());
        for (row, col, value) in entries {
            match compact.last_mut() {
                Some((last_row, last_col, last_value)) if *last_row == row && *last_col == col => {
                    *last_value += value;
                    if !last_value.is_finite() {
                        return Err(SparseMatrixError::NonFiniteValue);
                    }
                }
                _ => compact.push((row, col, value)),
            }
        }
        compact.retain(|&(_, _, value)| value != 0.0);
        let mut row_offsets = Vec::new();
        row_offsets
            .try_reserve_exact(offset_count)
            .map_err(|_| SparseMatrixError::Capacity)?;
        row_offsets.resize(offset_count, 0);
        for &(row, _, _) in &compact {
            row_offsets[row + 1] += 1;
        }
        for row in 1..=rows {
            row_offsets[row] += row_offsets[row - 1];
        }
        Ok(Self {
            rows,
            cols,
            row_offsets,
            column_indices: compact.iter().map(|&(_, col, _)| col).collect(),
            values: compact.iter().map(|&(_, _, value)| value).collect(),
        })
    }

    pub fn shape(&self) -> (usize, usize) {
        (self.rows, self.cols)
    }
    pub fn non_zero_count(&self) -> usize {
        self.values.len()
    }

    /// Bytes held by CSR vector capacities; excludes allocator metadata and
    /// temporary construction/algorithm buffers. Not process RSS.
    pub fn allocated_bytes(&self) -> usize {
        (self.row_offsets.capacity() + self.column_indices.capacity())
            * std::mem::size_of::<usize>()
            + self.values.capacity() * std::mem::size_of::<f32>()
    }

    /// Multiplies the sparse matrix by a dense vector without materializing
    /// zero entries.
    pub fn matvec(&self, vector: &[f32]) -> Result<Vec<f32>, SparseMatrixError> {
        if vector.len() != self.cols {
            return Err(SparseMatrixError::VectorLength {
                expected: self.cols,
                actual: vector.len(),
            });
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(SparseMatrixError::NonFiniteValue);
        }
        let mut output = vec![0.0; self.rows];
        for (row, slot) in output.iter_mut().enumerate() {
            let mut sum = 0.0_f64;
            for index in self.row_offsets[row]..self.row_offsets[row + 1] {
                sum +=
                    f64::from(self.values[index]) * f64::from(vector[self.column_indices[index]]);
            }
            *slot = sum as f32;
            if !slot.is_finite() {
                return Err(SparseMatrixError::NonFiniteValue);
            }
        }
        Ok(output)
    }

    /// Runs a small, allocation-bounded PageRank iteration over a square
    /// adjacency matrix. Edge values are treated as non-negative weights and
    /// normalized per source row; dangling rows distribute their rank evenly.
    pub fn pagerank(&self, iterations: usize, damping: f32) -> Result<Vec<f32>, SparseMatrixError> {
        Ok(self
            .pagerank_until(iterations, f64::from(damping), 0.0)?
            .scores
            .into_iter()
            .map(|value| value as f32)
            .collect())
    }

    /// Weighted PageRank with f64 accumulation and optional L1 convergence.
    /// A tolerance of zero performs exactly `max_iterations` iterations.
    /// Temporary algorithm storage is O(rows), with two reusable rank buffers.
    pub fn pagerank_until(
        &self,
        max_iterations: usize,
        damping: f64,
        tolerance: f64,
    ) -> Result<PageRankResult, SparseMatrixError> {
        if self.rows != self.cols {
            return Err(SparseMatrixError::NotSquare {
                rows: self.rows,
                cols: self.cols,
            });
        }
        if !(0.0..=1.0).contains(&damping) || !damping.is_finite() {
            return Err(SparseMatrixError::InvalidDamping);
        }
        if max_iterations == 0 || max_iterations > 1000 {
            return Err(SparseMatrixError::InvalidIterations);
        }
        if !tolerance.is_finite() || tolerance < 0.0 {
            return Err(SparseMatrixError::InvalidTolerance);
        }
        if self.values.iter().any(|value| *value < 0.0) {
            return Err(SparseMatrixError::NegativeWeight);
        }
        if self.rows == 0 {
            return Ok(PageRankResult {
                scores: vec![],
                iterations: 0,
                converged: true,
                residual: 0.0,
            });
        }
        let n = self.rows as f64;
        let mut rank = vec![1.0 / n; self.rows];
        let mut next = vec![0.0; self.rows];
        let totals: Vec<f64> = self
            .row_offsets
            .windows(2)
            .map(|range| {
                self.values[range[0]..range[1]]
                    .iter()
                    .map(|v| f64::from(*v))
                    .sum()
            })
            .collect();
        let mut residual = 0.0;
        for iteration in 1..=max_iterations {
            let dangling: f64 = totals
                .iter()
                .zip(&rank)
                .filter(|(total, _)| **total == 0.0)
                .map(|(_, rank)| rank)
                .sum();
            let base = (1.0 - damping) / n + damping * dangling / n;
            next.fill(base);
            for (row, source_rank) in rank.iter().copied().enumerate() {
                let start = self.row_offsets[row];
                let end = self.row_offsets[row + 1];
                let total = totals[row];
                if total == 0.0 {
                    continue;
                }
                for index in start..end {
                    let target = self.column_indices[index];
                    next[target] += damping * source_rank * (f64::from(self.values[index]) / total);
                }
            }
            residual = rank.iter().zip(&next).map(|(a, b)| (a - b).abs()).sum();
            std::mem::swap(&mut rank, &mut next);
            if tolerance > 0.0 && residual <= tolerance {
                return Ok(PageRankResult {
                    scores: rank,
                    iterations: iteration,
                    converged: true,
                    residual,
                });
            }
        }
        Ok(PageRankResult {
            scores: rank,
            iterations: max_iterations,
            converged: false,
            residual,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SparseMatrixError {
    #[error("matrix dimensions or allocation exceed addressable capacity")]
    Capacity,
    #[error("PageRank maxIterations must be between 1 and 1000")]
    InvalidIterations,
    #[error("PageRank tolerance must be finite and non-negative")]
    InvalidTolerance,
    #[error("matrix index ({row}, {col}) is out of bounds")]
    IndexOutOfBounds { row: usize, col: usize },
    #[error("matrix values must be finite")]
    NonFiniteValue,
    #[error("vector length {actual} does not match matrix columns {expected}")]
    VectorLength { expected: usize, actual: usize },
    #[error("matrix must be square for PageRank, got {rows}x{cols}")]
    NotSquare { rows: usize, cols: usize },
    #[error("PageRank damping must be finite and between 0 and 1")]
    InvalidDamping,
    #[error("PageRank does not accept negative edge weights")]
    NegativeWeight,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn pagerank_matches_independent_dense_reference(entries in prop::collection::vec((0usize..7, 0usize..7, 0u8..20), 0..60)) {
            let mut dense = [[0.0_f64; 7]; 7];
            for &(r, c, w) in &entries { dense[r][c] += f64::from(w); }
            let sparse = SparseMatrix::from_entries(7, 7, entries.iter().map(|&(r, c, w)| (r, c, f32::from(w)))).unwrap();
            let mut reference = [1.0 / 7.0; 7];
            for _ in 0..25 {
                let mut next = [0.15 / 7.0; 7];
                for r in 0..7 {
                    let total: f64 = dense[r].iter().sum();
                    for (c, slot) in next.iter_mut().enumerate() {
                        *slot += 0.85 * reference[r] * if total == 0.0 { 1.0 / 7.0 } else { dense[r][c] / total };
                    }
                }
                reference = next;
            }
            let result = sparse.pagerank_until(25, 0.85, 0.0).unwrap();
            prop_assert_eq!(result.iterations, 25);
            for (actual, expected) in result.scores.iter().zip(reference) {
                prop_assert!((actual - expected).abs() < 1e-12);
            }
            prop_assert!((result.scores.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn pagerank_validates_options_and_reports_convergence() {
        let square = SparseMatrix::from_entries(2, 2, [(0, 1, 1.0), (1, 0, 1.0)]).unwrap();
        let result = square.pagerank_until(100, 0.85, 1e-10).unwrap();
        assert_eq!(result.iterations, 1);
        assert!(result.converged);
        assert_eq!(result.residual, 0.0);
        for damping in [-1.0, 1.1, f64::NAN, f64::INFINITY] {
            assert!(square.pagerank_until(20, damping, 1e-8).is_err());
        }
        for tolerance in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(square.pagerank_until(20, 0.85, tolerance).is_err());
        }
        for iterations in [0, 1001] {
            assert!(square.pagerank_until(iterations, 0.85, 1e-8).is_err());
        }
        assert!(SparseMatrix::from_entries(2, 3, [])
            .unwrap()
            .pagerank(10, 0.85)
            .is_err());
        assert!(SparseMatrix::from_entries(1, 1, [(0, 0, -1.0)])
            .unwrap()
            .pagerank(10, 0.85)
            .is_err());
        assert!(SparseMatrix::from_entries(0, 0, [])
            .unwrap()
            .pagerank_until(1, 0.85, 0.0)
            .unwrap()
            .scores
            .is_empty());
    }

    #[test]
    fn zero_weights_and_cancellation_do_not_store_zero_coordinates() {
        let matrix =
            SparseMatrix::from_entries(2, 2, [(0, 1, 0.0), (1, 0, 2.0), (1, 0, -2.0)]).unwrap();
        assert_eq!(matrix.non_zero_count(), 0);
        let rank = matrix.pagerank(20, 0.85).unwrap();
        assert!((rank.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(rank, [0.5, 0.5]);
    }

    #[test]
    fn duplicate_overflow_is_an_error_and_large_row_sums_preserve_mass() {
        assert!(SparseMatrix::from_entries(1, 1, [(0, 0, f32::MAX), (0, 0, f32::MAX)]).is_err());
        let matrix =
            SparseMatrix::from_entries(3, 3, [(0, 1, f32::MAX), (0, 2, f32::MAX)]).unwrap();
        let ranks = matrix.pagerank(30, 0.85).unwrap();
        assert!(ranks.iter().all(|x| x.is_finite()));
        assert!((ranks.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(ranks[1] > ranks[0]);
    }

    #[test]
    fn impossible_shape_is_an_error_without_panicking() {
        assert!(SparseMatrix::from_entries(usize::MAX, 0, []).is_err());
    }

    #[test]
    fn invalid_matvec_inputs_and_overflow_are_errors() {
        let matrix = SparseMatrix::from_entries(1, 1, [(0, 0, f32::MAX)]).unwrap();
        assert!(matrix.matvec(&[f32::NAN]).is_err());
        assert!(matrix.matvec(&[2.0]).is_err());
    }

    #[test]
    fn builds_csr_and_sums_duplicate_edges() {
        let matrix =
            SparseMatrix::from_entries(3, 3, [(0, 1, 1.0), (0, 1, 2.0), (2, 0, 4.0)]).unwrap();
        assert_eq!(matrix.shape(), (3, 3));
        assert_eq!(matrix.non_zero_count(), 2);
        assert_eq!(matrix.matvec(&[2.0, 3.0, 5.0]).unwrap(), [9.0, 0.0, 8.0]);
    }

    #[test]
    fn rejects_bad_indices_and_vector_shape() {
        assert!(matches!(
            SparseMatrix::from_entries(2, 2, [(2, 0, 1.0)]),
            Err(SparseMatrixError::IndexOutOfBounds { .. })
        ));
        let matrix = SparseMatrix::from_entries(2, 2, []).unwrap();
        assert!(matches!(
            matrix.matvec(&[1.0]),
            Err(SparseMatrixError::VectorLength { .. })
        ));
    }

    #[test]
    fn pagerank_converges_on_a_small_graph() {
        let matrix =
            SparseMatrix::from_entries(3, 3, [(0, 1, 1.0), (1, 2, 1.0), (2, 1, 1.0)]).unwrap();
        let rank = matrix.pagerank(30, 0.85).unwrap();
        assert!(rank[1] > rank[0]);
        assert!(rank[1] > rank[2] * 0.9);
        assert!((rank.iter().sum::<f32>() - 1.0).abs() < 0.0001);
    }
}
