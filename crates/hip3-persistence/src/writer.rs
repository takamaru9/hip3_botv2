//! Parquet file writer for signal and market data.
//!
//! P0-1: Uses daily file aggregation - keeps ArrowWriter open until date rotation.

use crate::error::PersistenceResult;
use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::Utc;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use std::fs::File;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Signal record for persistence.
#[derive(Debug, Clone)]
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

/// P0-1: Active writer state for daily file aggregation.
/// Keeps the ArrowWriter open until date rotation to prevent file corruption.
struct ActiveWriter {
    writer: ArrowWriter<File>,
    date: String,
    schema: Arc<Schema>,
}

/// Parquet writer for signal records.
///
/// P0-1: Maintains an active ArrowWriter per day. Multiple flush() calls
/// within the same day append to the same writer, preventing file corruption.
pub struct ParquetWriter {
    /// Base directory for output files.
    base_dir: String,
    /// Buffer of pending records.
    buffer: Vec<SignalRecord>,
    /// Maximum buffer size before flush.
    max_buffer_size: usize,
    /// P0-1: Active writer (open until date rotation).
    active_writer: Option<ActiveWriter>,
}

impl ParquetWriter {
    /// Create a new Parquet writer.
    pub fn new(base_dir: &str, max_buffer_size: usize) -> Self {
        // Create directory if it doesn't exist
        std::fs::create_dir_all(base_dir).ok();

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

    /// Create the signal schema.
    fn create_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("timestamp_ms", DataType::Int64, false),
            Field::new("market_key", DataType::Utf8, false),
            Field::new("side", DataType::Utf8, false),
            Field::new("raw_edge_bps", DataType::Float64, false),
            Field::new("net_edge_bps", DataType::Float64, false),
            Field::new("oracle_px", DataType::Float64, false),
            Field::new("best_px", DataType::Float64, false),
            Field::new("suggested_size", DataType::Float64, false),
            Field::new("signal_id", DataType::Utf8, false),
        ]))
    }

    /// P0-1: Close the active writer and finalize the Parquet file.
    fn close_active_writer(&mut self) -> PersistenceResult<()> {
        if let Some(active) = self.active_writer.take() {
            info!(date = %active.date, "Closing Parquet writer for date rotation");
            active.writer.close()?;
        }
        Ok(())
    }

    /// P0-1: Create a new writer for the given date.
    fn create_new_writer(&mut self, date: &str) -> PersistenceResult<()> {
        let filename = format!("{}/signals_{}.parquet", self.base_dir, date);
        let schema = Self::create_schema();

        info!(filename = %filename, "Creating new Parquet writer");

        let file = File::options()
            .create(true)
            .write(true)
            .truncate(true) // Truncate on new day
            .open(&filename)?;

        let props = WriterProperties::builder()
            .set_compression(parquet::basic::Compression::SNAPPY)
            .build();

        let writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;

        self.active_writer = Some(ActiveWriter {
            writer,
            date: date.to_string(),
            schema,
        });

        Ok(())
    }

    /// Flush buffer to Parquet file.
    ///
    /// P0-1: Uses daily file aggregation. Keeps ArrowWriter open until
    /// date rotation to prevent file corruption from multiple writes.
    pub fn flush(&mut self) -> PersistenceResult<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let today = Utc::now().format("%Y-%m-%d").to_string();

        // P0-1: Check if date changed - rotate writer if needed
        let needs_rotation = self
            .active_writer
            .as_ref()
            .map(|w| w.date != today)
            .unwrap_or(false);

        if needs_rotation {
            self.close_active_writer()?;
        }

        // P0-1: Create new writer if none exists
        if self.active_writer.is_none() {
            self.create_new_writer(&today)?;
        }

        // Get schema (clone to avoid borrow conflict)
        let schema = self
            .active_writer
            .as_ref()
            .expect("active_writer should exist after create_new_writer")
            .schema
            .clone();

        let date_for_log = self
            .active_writer
            .as_ref()
            .map(|w| w.date.clone())
            .unwrap_or_default();

        info!(
            date = %date_for_log,
            records = self.buffer.len(),
            "Flushing signals to Parquet"
        );

        // Build arrays from buffer (uses immutable borrow of self.buffer)
        let batch = self.build_record_batch(&schema)?;

        // Now get mutable borrow of writer and write the batch
        let active = self
            .active_writer
            .as_mut()
            .expect("active_writer should exist");

        active.writer.write(&batch)?;
        // BUG-001 fix: Flush row group to disk after write.
        // Without this, data stays in memory until row group is full (1M rows)
        // or ArrowWriter::close() is called.
        active.writer.flush()?;

        debug!(
            date = %date_for_log,
            records = self.buffer.len(),
            "Parquet write complete"
        );

        self.buffer.clear();

        Ok(())
    }

    /// Close the writer, flushing any pending data and finalizing the Parquet file.
    ///
    /// BUG-001 fix: This must be called on graceful shutdown to ensure the
    /// Parquet footer is written. Without the footer, the file cannot be read.
    pub fn close(&mut self) -> PersistenceResult<()> {
        self.flush()?;
        self.close_active_writer()
    }

    /// Build a RecordBatch from the current buffer.
    fn build_record_batch(&self, schema: &Arc<Schema>) -> PersistenceResult<RecordBatch> {
        let timestamp_ms: ArrayRef = Arc::new(Int64Array::from(
            self.buffer
                .iter()
                .map(|r| r.timestamp_ms)
                .collect::<Vec<_>>(),
        ));
        let market_key: ArrayRef = Arc::new(StringArray::from(
            self.buffer
                .iter()
                .map(|r| r.market_key.as_str())
                .collect::<Vec<_>>(),
        ));
        let side: ArrayRef = Arc::new(StringArray::from(
            self.buffer
                .iter()
                .map(|r| r.side.as_str())
                .collect::<Vec<_>>(),
        ));
        let raw_edge_bps: ArrayRef = Arc::new(Float64Array::from(
            self.buffer
                .iter()
                .map(|r| r.raw_edge_bps)
                .collect::<Vec<_>>(),
        ));
        let net_edge_bps: ArrayRef = Arc::new(Float64Array::from(
            self.buffer
                .iter()
                .map(|r| r.net_edge_bps)
                .collect::<Vec<_>>(),
        ));
        let oracle_px: ArrayRef = Arc::new(Float64Array::from(
            self.buffer.iter().map(|r| r.oracle_px).collect::<Vec<_>>(),
        ));
        let best_px: ArrayRef = Arc::new(Float64Array::from(
            self.buffer.iter().map(|r| r.best_px).collect::<Vec<_>>(),
        ));
        let suggested_size: ArrayRef = Arc::new(Float64Array::from(
            self.buffer
                .iter()
                .map(|r| r.suggested_size)
                .collect::<Vec<_>>(),
        ));
        let signal_id: ArrayRef = Arc::new(StringArray::from(
            self.buffer
                .iter()
                .map(|r| r.signal_id.as_str())
                .collect::<Vec<_>>(),
        ));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                timestamp_ms,
                market_key,
                side,
                raw_edge_bps,
                net_edge_bps,
                oracle_px,
                best_px,
                suggested_size,
                signal_id,
            ],
        )?;

        Ok(batch)
    }
}

impl Drop for ParquetWriter {
    fn drop(&mut self) {
        // P0-1: Flush buffer and close active writer
        if let Err(e) = self.flush() {
            warn!(?e, "Failed to flush buffer on drop");
        }
        if let Err(e) = self.close_active_writer() {
            warn!(?e, "Failed to close writer on drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
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
    fn test_signal_record() {
        let record = SignalRecord {
            timestamp_ms: 1234567890000,
            market_key: "xyz:0".to_string(),
            side: "buy".to_string(),
            raw_edge_bps: 15.5,
            net_edge_bps: 5.5,
            oracle_px: 50000.0,
            best_px: 49990.0,
            suggested_size: 0.01,
            signal_id: "test_123".to_string(),
        };

        assert_eq!(record.market_key, "xyz:0");
    }

    /// P0-1: Test multiple flushes produce a valid Parquet file.
    #[test]
    fn test_multiple_flushes_same_day() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = ParquetWriter::new(temp_dir.path().to_str().unwrap(), 100);

        // First batch of records
        for i in 0..5 {
            writer.add_record(make_test_record(i)).unwrap();
        }
        writer.flush().unwrap();

        // Second batch of records (same day)
        for i in 5..10 {
            writer.add_record(make_test_record(i)).unwrap();
        }
        writer.flush().unwrap();

        // Third batch of records (same day)
        for i in 10..15 {
            writer.add_record(make_test_record(i)).unwrap();
        }
        writer.flush().unwrap();

        // Close the writer to finalize the file
        writer.close_active_writer().unwrap();

        // Find the Parquet file
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "Should have exactly one file");

        let file_path = entries[0].path();
        assert!(
            file_path.to_str().unwrap().contains("signals_"),
            "File should be named signals_*.parquet"
        );

        // Read and verify the Parquet file
        let file = File::open(&file_path).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();

        let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();

        // Count total rows
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 15, "Should have 15 records from 3 flushes");
    }

    /// P0-1: Test that empty flush is a no-op.
    #[test]
    fn test_empty_flush_noop() {
        let temp_dir = TempDir::new().unwrap();
        let mut writer = ParquetWriter::new(temp_dir.path().to_str().unwrap(), 100);

        // Flush with no records
        writer.flush().unwrap();

        // No file should be created
        let entries: Vec<_> = std::fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "No file should be created for empty flush"
        );
    }

    /// P0-1: Test that Drop properly closes the writer.
    #[test]
    fn test_drop_closes_writer() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_str().unwrap().to_string();

        {
            let mut writer = ParquetWriter::new(&temp_path, 100);

            // Add some records
            for i in 0..5 {
                writer.add_record(make_test_record(i)).unwrap();
            }

            // Don't call flush - let Drop handle it
        }

        // After drop, file should be readable
        let entries: Vec<_> = std::fs::read_dir(&temp_path)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "Should have exactly one file after drop");

        let file_path = entries[0].path();
        let file = File::open(&file_path).unwrap();

        // This should not fail - file should be properly closed
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();

        let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 5, "Should have 5 records");
    }
}
