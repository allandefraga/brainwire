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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentsResult {
    /// Minimum CSR node index of each node's component.
    pub components: Vec<usize>,
    /// Component size at its representative; zero at other indices.
    pub sizes: Vec<usize>,
}

struct ComponentCheck<F> {
    should_stop: F,
    remaining: usize,
}
impl<F: FnMut() -> bool> ComponentCheck<F> {
    fn check(&mut self) -> Result<(), SparseMatrixError> {
        if (self.should_stop)() {
            return Err(SparseMatrixError::Interrupted);
        }
        self.remaining = 1024;
        Ok(())
    }
    fn step(&mut self) -> Result<(), SparseMatrixError> {
        if self.remaining == 0 {
            self.check()?;
        }
        self.remaining -= 1;
        Ok(())
    }
    fn buffer<T>(&mut self, count: usize) -> Result<Vec<T>, SparseMatrixError> {
        self.check()?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| SparseMatrixError::Capacity)?;
        Ok(values)
    }
    fn filled<T: Copy>(&mut self, count: usize, value: T) -> Result<Vec<T>, SparseMatrixError> {
        let mut values = self.buffer(count)?;
        for _ in 0..count {
            self.step()?;
            values.push(value);
        }
        Ok(values)
    }
}

fn component_root<F: FnMut() -> bool>(
    parents: &mut [usize],
    mut node: usize,
    check: &mut ComponentCheck<F>,
) -> Result<usize, SparseMatrixError> {
    while parents[node] != node {
        check.step()?;
        parents[node] = parents[parents[node]];
        node = parents[node];
    }
    Ok(node)
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

    /// Directed, unweighted shortest hop depths over nonzero CSR entries.
    /// O(rows + visited entries) time and O(rows) temporary storage.
    pub fn breadth_first(
        &self,
        source: usize,
        max_depth: usize,
    ) -> Result<Vec<Option<usize>>, SparseMatrixError> {
        self.breadth_first_with(source, max_depth, |_, _| false)
    }

    /// One minimum-hop directed path; weights do not affect the chosen route.
    /// Ties follow CSR column order. No path within max_depth returns None.
    pub fn shortest_path(
        &self,
        source: usize,
        target: usize,
        max_depth: usize,
    ) -> Result<Option<Vec<usize>>, SparseMatrixError> {
        if target >= self.rows {
            return Err(SparseMatrixError::IndexOutOfBounds {
                row: target,
                col: target,
            });
        }
        self.validate_bfs_source(source)?;
        if source == target {
            return Ok(Some(vec![source]));
        }
        let mut parents = vec![None; self.rows];
        let depths = self.breadth_first_with(source, max_depth, |parent, node| {
            parents[node] = Some(parent);
            node == target
        })?;
        if depths[target].is_none() {
            return Ok(None);
        }
        let mut path = vec![target];
        let mut node = target;
        while node != source {
            node = parents[node].expect("visited non-source node has parent");
            path.push(node);
        }
        path.reverse();
        Ok(Some(path))
    }

    fn breadth_first_with(
        &self,
        source: usize,
        max_depth: usize,
        mut discover: impl FnMut(usize, usize) -> bool,
    ) -> Result<Vec<Option<usize>>, SparseMatrixError> {
        self.validate_bfs_source(source)?;
        let mut depths = vec![None; self.rows];
        let mut queue = std::collections::VecDeque::from([source]);
        depths[source] = Some(0);
        while let Some(row) = queue.pop_front() {
            let depth = depths[row].expect("queued node has a depth");
            if depth >= max_depth {
                continue;
            }
            for index in self.row_offsets[row]..self.row_offsets[row + 1] {
                let target = self.column_indices[index];
                if depths[target].is_none() {
                    depths[target] = Some(depth + 1);
                    if discover(row, target) {
                        return Ok(depths);
                    }
                    queue.push_back(target);
                }
            }
        }
        Ok(depths)
    }

    fn validate_bfs_source(&self, source: usize) -> Result<(), SparseMatrixError> {
        if self.rows != self.cols {
            return Err(SparseMatrixError::NotSquare {
                rows: self.rows,
                cols: self.cols,
            });
        }
        if source >= self.rows {
            return Err(SparseMatrixError::IndexOutOfBounds {
                row: source,
                col: source,
            });
        }
        Ok(())
    }

    /// Weak connectivity ignores direction and weights of nonzero CSR entries.
    /// Union by size and path compression use 3*N usize slots of requested
    /// backing storage, including results; allocator overhead is excluded.
    pub fn weak_components(&self) -> Result<ComponentsResult, SparseMatrixError> {
        self.weak_components_with_check(|| false)
    }

    /// Checks before allocation and after at most 1024 node/edge/root steps.
    pub fn weak_components_with_check(
        &self,
        should_stop: impl FnMut() -> bool,
    ) -> Result<ComponentsResult, SparseMatrixError> {
        self.validate_component_shape()?;
        let mut check = ComponentCheck {
            should_stop,
            remaining: 0,
        };
        let mut parents = check.buffer(self.rows)?;
        for node in 0..self.rows {
            check.step()?;
            parents.push(node);
        }
        let mut sizes = check.filled(self.rows, 1usize)?;
        for row in 0..self.rows {
            check.step()?;
            for edge in self.row_offsets[row]..self.row_offsets[row + 1] {
                check.step()?;
                let mut a = component_root(&mut parents, row, &mut check)?;
                let mut b = component_root(&mut parents, self.column_indices[edge], &mut check)?;
                if a != b {
                    if sizes[a] < sizes[b] {
                        std::mem::swap(&mut a, &mut b);
                    }
                    parents[b] = a;
                    sizes[a] += sizes[b];
                }
            }
        }
        let mut minima = check.filled(self.rows, usize::MAX)?;
        for node in 0..self.rows {
            check.step()?;
            let root = component_root(&mut parents, node, &mut check)?;
            parents[node] = root;
            minima[root] = minima[root].min(node);
        }
        for size in &mut sizes {
            check.step()?;
            *size = 0;
        }
        for parent in &mut parents {
            check.step()?;
            *parent = minima[*parent];
            sizes[*parent] += 1;
        }
        check.check()?;
        Ok(ComponentsResult {
            components: parents,
            sizes,
        })
    }

    /// Iterative Kosaraju traversal over nonzero directed topology, including isolates.
    /// Requested algorithm storage is at most 48*N + 8*NNZ + 8 bytes on 64-bit
    /// platforms, excluding the input CSR, allocator metadata and result clones.
    pub fn strong_components(&self) -> Result<ComponentsResult, SparseMatrixError> {
        self.strong_components_with_check(|| false)
    }

    /// Checks before allocation and after at most 1024 node/edge steps.
    pub fn strong_components_with_check(
        &self,
        should_stop: impl FnMut() -> bool,
    ) -> Result<ComponentsResult, SparseMatrixError> {
        self.validate_component_shape()?;
        let mut check = ComponentCheck {
            should_stop,
            remaining: 0,
        };
        let mut visited = check.filled(self.rows, false)?;
        let mut order = check.buffer(self.rows)?;
        let mut frames = check.buffer::<(usize, usize)>(self.rows)?;
        for start in 0..self.rows {
            check.step()?;
            if visited[start] {
                continue;
            }
            visited[start] = true;
            frames.push((start, self.row_offsets[start]));
            while let Some(&(node, edge)) = frames.last() {
                check.step()?;
                if edge < self.row_offsets[node + 1] {
                    frames.last_mut().expect("nonempty frames").1 += 1;
                    let target = self.column_indices[edge];
                    if !visited[target] {
                        visited[target] = true;
                        frames.push((target, self.row_offsets[target]));
                    }
                } else {
                    frames.pop();
                    order.push(node);
                }
            }
        }
        drop(visited);
        drop(frames);
        let offset_count = self
            .rows
            .checked_add(1)
            .ok_or(SparseMatrixError::Capacity)?;
        let mut offsets = check.filled(offset_count, 0usize)?;
        for &target in &self.column_indices {
            check.step()?;
            offsets[target + 1] += 1;
        }
        for node in 1..offset_count {
            check.step()?;
            offsets[node] += offsets[node - 1];
        }
        let mut next = check.buffer(self.rows)?;
        for &offset in &offsets[..self.rows] {
            check.step()?;
            next.push(offset);
        }
        let mut targets = check.filled(self.column_indices.len(), 0usize)?;
        for row in 0..self.rows {
            check.step()?;
            for edge in self.row_offsets[row]..self.row_offsets[row + 1] {
                check.step()?;
                let target = self.column_indices[edge];
                targets[next[target]] = row;
                next[target] += 1;
            }
        }
        drop(next);
        let mut components = check.filled(self.rows, usize::MAX)?;
        let mut sizes = check.filled(self.rows, 0usize)?;
        let mut minima = check.filled(self.rows, usize::MAX)?;
        let mut stack = check.buffer(self.rows)?;
        for &start in order.iter().rev() {
            check.step()?;
            if components[start] != usize::MAX {
                continue;
            }
            components[start] = start;
            stack.push(start);
            while let Some(node) = stack.pop() {
                check.step()?;
                minima[start] = minima[start].min(node);
                sizes[start] += 1;
                for &target in &targets[offsets[node]..offsets[node + 1]] {
                    check.step()?;
                    if components[target] == usize::MAX {
                        components[target] = start;
                        stack.push(target);
                    }
                }
            }
        }
        for component in &mut components {
            check.step()?;
            *component = minima[*component];
        }
        for root in 0..self.rows {
            check.step()?;
            if sizes[root] != 0 && minima[root] != root {
                sizes[minima[root]] = sizes[root];
                sizes[root] = 0;
            }
        }
        check.check()?;
        Ok(ComponentsResult { components, sizes })
    }

    fn validate_component_shape(&self) -> Result<(), SparseMatrixError> {
        if self.rows != self.cols {
            return Err(SparseMatrixError::NotSquare {
                rows: self.rows,
                cols: self.cols,
            });
        }
        Ok(())
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
    #[error("graph component traversal interrupted")]
    Interrupted,
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
    #[error("matrix must be square for graph algorithms, got {rows}x{cols}")]
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

    #[test]
    fn shortest_path_returns_minimum_hops_and_handles_cycles_bounds_and_isolation() {
        let matrix = SparseMatrix::from_entries(
            5,
            5,
            [
                (0, 1, 2.0),
                (1, 2, 1.0),
                (0, 3, 1.0),
                (3, 2, 1.0),
                (2, 0, 1.0),
            ],
        )
        .unwrap();
        assert_eq!(matrix.shortest_path(0, 2, 2).unwrap(), Some(vec![0, 1, 2]));
        assert_eq!(matrix.shortest_path(0, 0, 0).unwrap(), Some(vec![0]));
        assert_eq!(matrix.shortest_path(0, 2, 1).unwrap(), None);
        assert_eq!(matrix.shortest_path(0, 4, 10).unwrap(), None);
        assert!(matrix.shortest_path(0, 5, 2).is_err());
        assert!(SparseMatrix::from_entries(2, 3, [])
            .unwrap()
            .shortest_path(0, 1, 1)
            .is_err());
    }

    #[test]
    fn breadth_first_finds_shortest_directed_depths_and_stops_at_limit() {
        let graph = SparseMatrix::from_entries(
            6,
            6,
            [
                (0, 1, 1.0),
                (0, 2, 1.0),
                (1, 3, 1.0),
                (2, 3, 1.0),
                (3, 0, 1.0),
                (3, 4, 1.0),
            ],
        )
        .unwrap();
        assert_eq!(
            graph.breadth_first(0, 10).unwrap(),
            vec![Some(0), Some(1), Some(1), Some(2), Some(3), None]
        );
        assert_eq!(
            graph.breadth_first(0, 1).unwrap(),
            vec![Some(0), Some(1), Some(1), None, None, None]
        );
        assert_eq!(
            graph.breadth_first(0, 0).unwrap(),
            vec![Some(0), None, None, None, None, None]
        );
        assert_eq!(
            graph.breadth_first(5, 10).unwrap(),
            vec![None, None, None, None, None, Some(0)]
        );
        assert!(graph.breadth_first(6, 10).is_err());
        assert!(SparseMatrix::from_entries(2, 3, [])
            .unwrap()
            .breadth_first(0, 10)
            .is_err());
    }

    proptest! {
        #[test]
        fn breadth_first_matches_independent_all_pairs_shortest_hops(
            entries in prop::collection::vec((0usize..7, 0usize..7, 0u8..3), 0..50),
            source in 0usize..7, max_depth in 0usize..8,
        ) {
            let mut hops = [[16usize; 7]; 7];
            for (node, row) in hops.iter_mut().enumerate() { row[node] = 0; }
            for &(from, to, weight) in &entries {
                if weight > 0 { hops[from][to] = hops[from][to].min(1); }
            }
            for via in 0..7 {
                for from in 0..7 {
                    for to in 0..7 {
                        hops[from][to] = hops[from][to].min(hops[from][via] + hops[via][to]);
                    }
                }
            }
            let sparse = SparseMatrix::from_entries(7, 7, entries.iter().map(|&(from, to, weight)| (from, to, f32::from(weight)))).unwrap();
            let expected: Vec<_> = hops[source].iter().map(|depth| (*depth <= max_depth).then_some(*depth)).collect();
            prop_assert_eq!(sparse.breadth_first(source, max_depth).unwrap(), expected.clone());
            for (target,depth) in expected.iter().enumerate() {
                let path=sparse.shortest_path(source,target,max_depth).unwrap();
                prop_assert_eq!(path.as_ref().map(|p|p.len()-1),*depth);
                if let Some(path)=path { prop_assert_eq!(path[0],source); prop_assert_eq!(*path.last().unwrap(),target);
                    for pair in path.windows(2) { prop_assert!(entries.iter().any(|&(from,to,weight)|from==pair[0]&&to==pair[1]&&weight>0)); }
                }
            }
        }

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
    #[test]
    fn components_are_canonical_and_include_isolates_self_loops_and_directed_cycles() {
        let matrix = SparseMatrix::from_entries(
            7,
            7,
            [
                (0, 1, 1.0),
                (1, 0, 2.0),
                (1, 2, -1.0),
                (2, 3, 1.0),
                (3, 2, 1.0),
                (4, 4, 1.0),
                (0, 1, 1.0),
            ],
        )
        .unwrap();
        assert_eq!(
            matrix.weak_components().unwrap(),
            ComponentsResult {
                components: vec![0, 0, 0, 0, 4, 5, 6],
                sizes: vec![4, 0, 0, 0, 1, 1, 1]
            }
        );
        assert_eq!(
            matrix.strong_components().unwrap(),
            ComponentsResult {
                components: vec![0, 0, 2, 2, 4, 5, 6],
                sizes: vec![2, 0, 2, 0, 1, 1, 1]
            }
        );
        let empty = SparseMatrix::from_entries(0, 0, []).unwrap();
        assert!(empty.weak_components().unwrap().components.is_empty());
        assert!(empty.strong_components().unwrap().sizes.is_empty());
        let nonsquare = SparseMatrix::from_entries(2, 3, []).unwrap();
        assert!(matches!(
            nonsquare.weak_components(),
            Err(SparseMatrixError::NotSquare { .. })
        ));
        assert!(matches!(
            nonsquare.strong_components(),
            Err(SparseMatrixError::NotSquare { .. })
        ));
    }

    #[test]
    fn components_handle_100k_chain_and_ring_without_recursive_dfs() {
        let n = 100_000;
        let chain = SparseMatrix::from_entries(n, n, (0..n - 1).map(|i| (i, i + 1, 1.0))).unwrap();
        assert_eq!(chain.weak_components().unwrap().sizes[0], n);
        assert_eq!(
            chain.strong_components().unwrap().components,
            (0..n).collect::<Vec<_>>()
        );
        let ring = SparseMatrix::from_entries(n, n, (0..n).map(|i| (i, (i + 1) % n, 1.0))).unwrap();
        let result = ring.strong_components().unwrap();
        assert_eq!(result.components, vec![0; n]);
        assert_eq!(result.sizes[0], n);
    }

    #[test]
    fn component_checks_interrupt_before_allocation_and_during_high_degree_work() {
        let matrix =
            SparseMatrix::from_entries(10_000, 10_000, (1..10_000).map(|i| (0, i, 1.0))).unwrap();
        assert_eq!(
            matrix.weak_components_with_check(|| true),
            Err(SparseMatrixError::Interrupted)
        );
        assert_eq!(
            matrix.strong_components_with_check(|| true),
            Err(SparseMatrixError::Interrupted)
        );
        for limit in [2, 10, 30, 60] {
            let mut calls = 0;
            assert_eq!(
                matrix.weak_components_with_check(|| {
                    calls += 1;
                    calls > limit
                }),
                Err(SparseMatrixError::Interrupted)
            );
            let mut calls = 0;
            assert_eq!(
                matrix.strong_components_with_check(|| {
                    calls += 1;
                    calls > limit
                }),
                Err(SparseMatrixError::Interrupted)
            );
        }
    }

    proptest! {
        #[test]
        fn components_match_independent_dense_reachability(entries in prop::collection::vec((0usize..7,0usize..7),0..50)) {
            let matrix=SparseMatrix::from_entries(7,7,entries.iter().map(|&(a,b)|(a,b,1.0))).unwrap();
            let mut directed=[[false;7];7];let mut undirected=[[false;7];7];
            for i in 0..7 {directed[i][i]=true;undirected[i][i]=true;}
            for &(a,b) in &entries {directed[a][b]=true;undirected[a][b]=true;undirected[b][a]=true;}
            for via in 0..7 {for a in 0..7 {for b in 0..7 {directed[a][b]|=directed[a][via]&&directed[via][b];undirected[a][b]|=undirected[a][via]&&undirected[via][b];}}}
            for (result,strong) in [(matrix.weak_components().unwrap(),false),(matrix.strong_components().unwrap(),true)] {
                let mut expected=vec![0;7];let mut sizes=vec![0;7];
                for a in 0..7 {expected[a]=(0..7).find(|&b|if strong{directed[a][b]&&directed[b][a]}else{undirected[a][b]}).unwrap();sizes[expected[a]]+=1;}
                prop_assert_eq!(result.components,expected);prop_assert_eq!(result.sizes,sizes);
            }
        }
    }
}
