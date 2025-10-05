//! GPU Buffer Manager - prevents buffer overflows through proper accounting

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferType {
    MainText,    // Editor text content
    LineNumbers, // Line numbers + UI overlays
    Diagnostics, // Error/warning underlines
}

/// Manages GPU buffer allocation and prevents overflows
pub struct GpuBufferManager {
    /// Buffer capacities in bytes
    capacities: HashMap<BufferType, usize>,
    /// Current usage in bytes
    usage: HashMap<BufferType, usize>,
    /// Per-widget budget limits
    widget_limits: HashMap<String, usize>,
}

impl GpuBufferManager {
    pub fn new() -> Self {
        let mut capacities = HashMap::new();
        capacities.insert(BufferType::MainText, 4 * 1024 * 1024); // 4MB
        capacities.insert(BufferType::LineNumbers, 1024 * 1024); // 1MB
        capacities.insert(BufferType::Diagnostics, 512 * 1024); // 512KB

        let mut widget_limits = HashMap::new();
        widget_limits.insert("file_picker".to_string(), 100_000); // 100KB max
        widget_limits.insert("line_numbers".to_string(), 500_000); // 500KB max
        widget_limits.insert("diagnostics".to_string(), 200_000); // 200KB max

        Self {
            capacities,
            usage: HashMap::new(),
            widget_limits,
        }
    }

    /// Reserve space in a buffer for a widget
    /// Returns Ok(ReservationToken) if space available, Err if exceeded
    pub fn reserve(
        &mut self,
        buffer_type: BufferType,
        widget_id: &str,
        byte_count: usize,
    ) -> Result<ReservationToken, BufferError> {
        // Check widget budget
        if let Some(&limit) = self.widget_limits.get(widget_id) {
            if byte_count > limit {
                return Err(BufferError::WidgetBudgetExceeded {
                    widget: widget_id.to_string(),
                    requested: byte_count,
                    limit,
                });
            }
        }

        // Check buffer capacity
        let capacity = self.capacities.get(&buffer_type).copied().unwrap_or(0);
        let current_usage = self.usage.get(&buffer_type).copied().unwrap_or(0);

        if current_usage + byte_count > capacity {
            return Err(BufferError::BufferFull {
                buffer: buffer_type,
                current: current_usage,
                requested: byte_count,
                capacity,
            });
        }

        // Reserve the space
        *self.usage.entry(buffer_type).or_insert(0) += byte_count;

        Ok(ReservationToken {
            buffer_type,
            byte_count,
        })
    }

    /// Clear usage counters for next frame
    pub fn reset_frame(&mut self) {
        self.usage.clear();
    }

    /// Get available space in bytes
    pub fn available_space(&self, buffer_type: BufferType) -> usize {
        let capacity = self.capacities.get(&buffer_type).copied().unwrap_or(0);
        let usage = self.usage.get(&buffer_type).copied().unwrap_or(0);
        capacity.saturating_sub(usage)
    }
}

/// Token proving space was reserved
pub struct ReservationToken {
    pub buffer_type: BufferType,
    pub byte_count: usize,
}

#[derive(Debug)]
pub enum BufferError {
    WidgetBudgetExceeded {
        widget: String,
        requested: usize,
        limit: usize,
    },
    BufferFull {
        buffer: BufferType,
        current: usize,
        requested: usize,
        capacity: usize,
    },
}

impl std::fmt::Display for BufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BufferError::WidgetBudgetExceeded {
                widget,
                requested,
                limit,
            } => {
                write!(
                    f,
                    "Widget '{}' exceeded budget: requested {} bytes, limit {} bytes",
                    widget, requested, limit
                )
            }
            BufferError::BufferFull {
                buffer,
                current,
                requested,
                capacity,
            } => {
                write!(
                    f,
                    "Buffer {:?} full: {} bytes used + {} bytes requested > {} bytes capacity",
                    buffer, current, requested, capacity
                )
            }
        }
    }
}

impl std::error::Error for BufferError {}
