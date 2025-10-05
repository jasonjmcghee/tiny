//! GPU helper utilities for plugins to reduce boilerplate

use crate::ffi::BufferId;
use ahash::AHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// Helper for hash-based buffer caching
///
/// Automatically caches vertex/instance data and only writes to GPU when content changes.
/// This eliminates redundant GPU writes and reduces boilerplate in plugins.
///
/// # Example
/// ```rust
/// let buffer = CachedBuffer::new(1024, BufferUsages::VERTEX | BufferUsages::COPY_DST);
///
/// // In render loop:
/// let vertices = generate_vertices();
/// let cache_key = (pos.x, pos.y, color);
/// buffer.write_if_changed(bytemuck::cast_slice(&vertices), &cache_key);
/// ```
pub struct CachedBuffer {
    buffer_id: BufferId,
    last_hash: AtomicU64,
}

impl CachedBuffer {
    /// Create a new cached buffer with the given size and usage flags
    pub fn new(size: u64, usage: wgpu::BufferUsages) -> Self {
        Self {
            buffer_id: BufferId::create(size, usage),
            last_hash: AtomicU64::new(0),
        }
    }

    /// Write data to buffer only if the cache key has changed
    ///
    /// Returns true if data was written, false if cached value was used.
    ///
    /// # Example
    /// ```rust
    /// let cache_key = (position.x, position.y, color);
    /// if buffer.write_if_changed(vertex_data, &cache_key) {
    ///     println!("Vertices regenerated!");
    /// }
    /// ```
    pub fn write_if_changed<T: Hash>(&self, data: &[u8], cache_key: &T) -> bool {
        let mut hasher = AHasher::default();
        cache_key.hash(&mut hasher);
        let new_hash = hasher.finish();

        let old_hash = self.last_hash.load(Ordering::Relaxed);
        if old_hash != new_hash {
            self.buffer_id.write(0, data);
            self.last_hash.store(new_hash, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Get the underlying buffer ID for rendering
    pub fn buffer_id(&self) -> BufferId {
        self.buffer_id
    }
}
