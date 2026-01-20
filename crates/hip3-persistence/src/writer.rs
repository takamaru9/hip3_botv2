//! JSON Lines file writer for signal and market data.
//!
//! Uses JSON Lines format (.jsonl) for robustness:
//! - Each line is a complete JSON object
//! - Partial file corruption only affects individual lines
//! - Can be read even if write was interrupted
//! - Easy to convert to Parquet later if needed

use crate::error::PersistenceResult;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use tracing::{debug, info, warn};

/// Signal record for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalRecord {
    pub timestamp_ms: i64,
    pub market_key: String,
    pub side: String,
    pub raw_edge_bps: f64,
    pub net_edge_bps: f64,
    pub oracle_px: f64,
    pub best_px: f64,
    pub suggested_size: f64,
    pub signal_id: String,
}

/// Active writer state for daily file.
struct ActiveWriter {
    writer: BufWriter<File>,
    date: String,
    records_written: usize,
}

/// JSON Lines writer for signal records.
///
/// Uses append mode - safe for interrupted writes.
/// Each line is independent, so partial corruption only affects that line.
pub struct JsonLinesWriter {
    /// Base directory for output files.
    base_dir: String,
    /// Buffer of pending records.
    buffer: Vec<SignalRecord>,
    /// Maximum buffer size before flush.
    max_buffer_size: usize,
    /// Active writer (open until date rotation).
    active_writer: Option<ActiveWriter>,
}

impl JsonLinesWriter {
    /// Create a new JSON Lines writer.
    pub fn new(base_dir: &str, max_buffer_size: usize) -> Self {
        // Create directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(base_dir) {
            warn!(?e, "Failed to create directory: {}", base_dir);
        }

        Self {
            base_dir: base_dir.to_string(),
            buffer: Vec::with_capacity(max_buffer_size),
            max_buffer_size,
            active_writer: None,
        }
    }

    /// Add a signal record to the buffer.
    pub fn add_record(&mut self, record: SignalRecord) -> PersistenceResult<()> {
        self.buffer.push(record);

        if self.buffer.len() >= self.max_buffer_size {
            self.flush()?;
        }

        Ok(())
    }

    /// Close the active writer.
    fn close_active_writer(&mut self) -> PersistenceResult<()> {
        if let Some(mut active) = self.active_writer.take() {
            // Flush the BufWriter
            if let Err(e) = active.writer.flush() {
                warn!(?e, "Failed to flush writer on close");
            }
            info!(
                date = %active.date,
                records = active.records_written,
                "Closed JSON Lines writer"
            );
        }
        Ok(())
    }

    /// Create a new writer for the given date.
    fn create_new_writer(&mut self, date: &str) -> PersistenceResult<()> {
        let filename = format!("{}/signals_{}.jsonl", self.base_dir, date);

        info!(filename = %filename, "Opening JSON Lines writer (append mode)");

        // Open in append mode - won't truncate existing data
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&filename)?;

        let writer = BufWriter::new(file);

        self.active_writer = Some(ActiveWriter {
            writer,
            date: date.to_string(),
            records_written: 0,
        });

        Ok(())
    }

    /// Flush buffer to JSON Lines file.
    pub fn flush(&mut self) -> PersistenceResult<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let today = Utc::now().format("%Y-%m-%d").to_string();

        // Check if date changed - rotate writer if needed
        let needs_rotation = self
            .active_writer
            .as_ref()
            .map(|w| w.date != today)
            .unwrap_or(false);

        if needs_rotation {
            self.close_active_writer()?;
        }

        // Create new writer if none exists
        if self.active_writer.is_none() {
            self.create_new_writer(&today)?;
        }

        let record_count = self.buffer.len();

        // Write each record as a JSON line
        {
            let active = self
                .active_writer
                .as_mut()
                .expect("active_writer should exist");

            for record in &self.buffer {
                // Serialize to JSON
                let json = serde_json::to_string(record)?;
                // Write line
                writeln!(active.writer, "{}", json)?;
            }

            // Flush to disk immediately
            active.writer.flush()?;
            active.records_written += record_count;
        }

        debug!(
            date = %today,
            records = record_count,
            "Flushed signals to JSON Lines"
        );

        self.buffer.clear();

        Ok(())
    }

    /// Close the writer, flushing any pending data.
    pub fn close(&mut self) -> PersistenceResult<()> {
        self.flush()?;
        self.close_active_writer()
    }
}

impl Drop for JsonLinesWriter {
    fn drop(&mut self) {
        if let Err(e) = self.flush() {
            warn!(?e, "Failed to flush buffer on drop");
        }
        if let Err(e) = self.close_active_writer() {
            warn!(?e, "Failed to close writer on drop");
        }
    }
}

// Keep backward compatibility with old name
pub type ParquetWriter = JsonLinesWriter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use tempfile::TempDir;

    fn make_test_record(id: i64) -> SignalRecord {
        SignalRecord {
            timestamp_ms: 1234567890000 + id,
            market_key: "xyz:0".to_string(),
            side: "buy".to_string(),
            raw_edge_bps: 15.5,
            net_edge_bps: 5.5,
            oracle_px: 50000.0,
            best_px: 49990.0,
            suggested_size: 0.01,
            signal_id: format!("test_{}", id),
        }
    }

    #[test]
    fn test_write_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = JsonLinesWriter::new(temp_dir.path().to_str().unwrap(), 100);

        // Write records
        for i in 0..5 {
            writer.add_record(make_test_record(i)).unwrap();
        }
        writer.flush().unwrap();

        // Close writer
        writer.close().unwrap();

        // Find the file
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);

        // Read and verify
        let file = File::open(entries[0].path()).unwrap();
        let reader = BufReader::new(file);
        let lines: Vec<_> = reader.lines().filter_map(|l| l.ok()).collect();

        assert_eq!(lines.len(), 5);

        // Parse first line
        let record: SignalRecord = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(record.market_key, "xyz:0");
        assert_eq!(record.signal_id, "test_0");
    }

    #[test]
    fn test_append_mode() {
        let temp_dir = TempDir::new().unwrap();

        // First write
        {
            let mut writer = JsonLinesWriter::new(temp_dir.path().to_str().unwrap(), 100);
            for i in 0..3 {
                writer.add_record(make_test_record(i)).unwrap();
            }
            writer.close().unwrap();
        }

        // Second write (should append, not overwrite)
        {
            let mut writer = JsonLinesWriter::new(temp_dir.path().to_str().unwrap(), 100);
            for i in 3..6 {
                writer.add_record(make_test_record(i)).unwrap();
            }
            writer.close().unwrap();
        }

        // Verify total count
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        let file = File::open(entries[0].path()).unwrap();
        let reader = BufReader::new(file);
        let lines: Vec<_> = reader.lines().filter_map(|l| l.ok()).collect();

        assert_eq!(lines.len(), 6, "Should have 6 records total from 2 writes");
    }

    #[test]
    fn test_multiple_flushes() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = JsonLinesWriter::new(temp_dir.path().to_str().unwrap(), 100);

        // Multiple flushes
        for batch in 0..3 {
            for i in 0..5 {
                writer
                    .add_record(make_test_record(batch * 5 + i))
                    .unwrap();
            }
            writer.flush().unwrap();
        }
        writer.close().unwrap();

        // Verify
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        let file = File::open(entries[0].path()).unwrap();
        let reader = BufReader::new(file);
        let lines: Vec<_> = reader.lines().filter_map(|l| l.ok()).collect();

        assert_eq!(lines.len(), 15);
    }

    #[test]
    fn test_empty_flush_noop() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = JsonLinesWriter::new(temp_dir.path().to_str().unwrap(), 100);

        // Flush with no records
        writer.flush().unwrap();

        // No file should be created
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty());
    }
}
