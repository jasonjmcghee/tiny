//! Cluster mapping for shaped text
//!
//! Maps byte positions in source text to shaped glyphs and vice versa.
//! Handles ligatures (multiple bytes → 1 glyph) and complex clusters (1 byte → multiple glyphs).

use crate::shaping::ShapedGlyph;

/// Cluster info: maps a source byte range to glyph indices
#[derive(Clone, Debug)]
struct Cluster {
    /// Start byte position in source text
    byte_start: usize,
    /// End byte position in source text (exclusive)
    byte_end: usize,
    /// Indices into the glyphs array
    glyph_start: usize,
    /// Number of glyphs in this cluster
    glyph_count: usize,
}

/// Maps byte positions in source text to shaped glyphs
#[derive(Clone, Debug)]
pub struct ClusterMap {
    /// Clusters in byte order
    clusters: Vec<Cluster>,
    /// Total number of bytes in source text
    text_len: usize,
}

impl ClusterMap {
    /// Build a cluster map from shaped glyphs
    pub fn from_glyphs(glyphs: &[ShapedGlyph], text_len: usize) -> Self {
        let mut clusters = Vec::new();

        if glyphs.is_empty() {
            return Self {
                clusters,
                text_len,
            };
        }

        // Group glyphs by cluster
        let mut current_cluster_byte = glyphs[0].cluster as usize;
        let mut cluster_start_idx = 0;

        for (i, glyph) in glyphs.iter().enumerate() {
            let glyph_cluster_byte = glyph.cluster as usize;

            // If we've moved to a new cluster, finalize the previous one
            if glyph_cluster_byte != current_cluster_byte {
                let glyph_count = i - cluster_start_idx;

                // Calculate byte range for this cluster
                let byte_end = glyph_cluster_byte;

                clusters.push(Cluster {
                    byte_start: current_cluster_byte,
                    byte_end,
                    glyph_start: cluster_start_idx,
                    glyph_count,
                });

                current_cluster_byte = glyph_cluster_byte;
                cluster_start_idx = i;
            }
        }

        // Finalize the last cluster
        let glyph_count = glyphs.len() - cluster_start_idx;
        clusters.push(Cluster {
            byte_start: current_cluster_byte,
            byte_end: text_len, // Last cluster goes to end of text
            glyph_start: cluster_start_idx,
            glyph_count,
        });

        Self {
            clusters,
            text_len,
        }
    }

    /// Find the glyph index that corresponds to a byte position
    /// Returns (glyph_index, is_leading_edge)
    /// - glyph_index: the glyph at or before this byte position
    /// - is_leading_edge: true if cursor should be at glyph start, false if at end
    pub fn byte_to_glyph(&self, byte_pos: usize) -> Option<(usize, bool)> {
        if byte_pos >= self.text_len {
            // After end of text - place cursor after last glyph
            if let Some(last_cluster) = self.clusters.last() {
                let last_glyph = last_cluster.glyph_start + last_cluster.glyph_count - 1;
                return Some((last_glyph, false)); // Trailing edge
            }
            return None;
        }

        // Binary search for the cluster containing this byte
        match self.clusters.binary_search_by(|cluster| {
            if byte_pos < cluster.byte_start {
                std::cmp::Ordering::Greater
            } else if byte_pos >= cluster.byte_end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(cluster_idx) => {
                let cluster = &self.clusters[cluster_idx];

                // If we're at the exact start of the cluster, use leading edge of first glyph
                if byte_pos == cluster.byte_start {
                    Some((cluster.glyph_start, true))
                } else {
                    // Otherwise, use trailing edge of last glyph in cluster
                    let last_glyph = cluster.glyph_start + cluster.glyph_count - 1;
                    Some((last_glyph, false))
                }
            }
            Err(_) => {
                // Shouldn't happen if clusters are built correctly
                None
            }
        }
    }

    /// Find the byte position that corresponds to a glyph index
    /// Returns the start byte of the cluster containing this glyph
    pub fn glyph_to_byte(&self, glyph_idx: usize) -> Option<usize> {
        for cluster in &self.clusters {
            let glyph_end = cluster.glyph_start + cluster.glyph_count;
            if glyph_idx >= cluster.glyph_start && glyph_idx < glyph_end {
                return Some(cluster.byte_start);
            }
        }
        None
    }

    /// Get the byte range for a cluster containing a glyph
    pub fn glyph_to_byte_range(&self, glyph_idx: usize) -> Option<(usize, usize)> {
        for cluster in &self.clusters {
            let glyph_end = cluster.glyph_start + cluster.glyph_count;
            if glyph_idx >= cluster.glyph_start && glyph_idx < glyph_end {
                return Some((cluster.byte_start, cluster.byte_end));
            }
        }
        None
    }

    /// Check if a byte position is at a cluster boundary (can place cursor here)
    pub fn is_cluster_boundary(&self, byte_pos: usize) -> bool {
        if byte_pos == 0 || byte_pos >= self.text_len {
            return true;
        }

        self.clusters
            .iter()
            .any(|cluster| cluster.byte_start == byte_pos)
    }

    /// Find the nearest valid cursor position (cluster boundary) to a byte position
    pub fn snap_to_cluster_boundary(&self, byte_pos: usize) -> usize {
        if byte_pos >= self.text_len {
            return self.text_len;
        }

        // Find the cluster containing or after this position
        for cluster in &self.clusters {
            if byte_pos <= cluster.byte_start {
                return cluster.byte_start;
            }
            if byte_pos < cluster.byte_end {
                // Inside a cluster - snap to start or end depending on which is closer
                let dist_to_start = byte_pos - cluster.byte_start;
                let dist_to_end = cluster.byte_end - byte_pos;
                return if dist_to_start <= dist_to_end {
                    cluster.byte_start
                } else {
                    cluster.byte_end
                };
            }
        }

        // After all clusters
        self.text_len
    }

    /// Get the total number of glyphs
    pub fn glyph_count(&self) -> usize {
        self.clusters
            .last()
            .map(|c| c.glyph_start + c.glyph_count)
            .unwrap_or(0)
    }

    /// Get the number of clusters
    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
    }

    /// Check if a cluster is a ligature (multiple bytes → 1 glyph)
    pub fn is_ligature(&self, cluster_idx: usize) -> bool {
        if let Some(cluster) = self.clusters.get(cluster_idx) {
            let byte_count = cluster.byte_end - cluster.byte_start;
            byte_count > 1 && cluster.glyph_count == 1
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaping::ShapedGlyph;

    fn make_glyph(glyph_id: u16, cluster: u32) -> ShapedGlyph {
        ShapedGlyph {
            glyph_id,
            cluster,
            x_offset: 0.0,
            y_offset: 0.0,
            x_advance: 10.0,
            y_advance: 0.0,
        }
    }

    #[test]
    fn test_simple_mapping() {
        // Simple 1:1 mapping: "abc" -> 3 glyphs
        let glyphs = vec![
            make_glyph(1, 0), // 'a' at byte 0
            make_glyph(2, 1), // 'b' at byte 1
            make_glyph(3, 2), // 'c' at byte 2
        ];
        let map = ClusterMap::from_glyphs(&glyphs, 3);

        assert_eq!(map.byte_to_glyph(0), Some((0, true)));
        assert_eq!(map.byte_to_glyph(1), Some((1, true)));
        assert_eq!(map.byte_to_glyph(2), Some((2, true)));

        assert_eq!(map.glyph_to_byte(0), Some(0));
        assert_eq!(map.glyph_to_byte(1), Some(1));
        assert_eq!(map.glyph_to_byte(2), Some(2));
    }

    #[test]
    fn test_ligature_mapping() {
        // Ligature: "ffi" (3 bytes) -> 1 glyph
        // Cluster 0 covers bytes 0-3, has 1 glyph
        let glyphs = vec![
            make_glyph(1, 0), // ligature glyph for "ffi"
        ];
        let map = ClusterMap::from_glyphs(&glyphs, 3);

        // All bytes in the ligature should map to the same glyph
        assert_eq!(map.byte_to_glyph(0), Some((0, true)));
        assert_eq!(map.byte_to_glyph(1), Some((0, false)));
        assert_eq!(map.byte_to_glyph(2), Some((0, false)));

        // Glyph should map back to start of cluster
        assert_eq!(map.glyph_to_byte(0), Some(0));

        // Check it's recognized as a ligature
        assert!(map.is_ligature(0));
    }

    #[test]
    fn test_cluster_boundaries() {
        let glyphs = vec![make_glyph(1, 0), make_glyph(2, 3)];
        let map = ClusterMap::from_glyphs(&glyphs, 6);

        assert!(map.is_cluster_boundary(0));
        assert!(!map.is_cluster_boundary(1));
        assert!(!map.is_cluster_boundary(2));
        assert!(map.is_cluster_boundary(3));

        // Snapping should move to nearest boundary
        assert_eq!(map.snap_to_cluster_boundary(1), 0);
        assert_eq!(map.snap_to_cluster_boundary(2), 3);
        assert_eq!(map.snap_to_cluster_boundary(4), 3);
    }
}
