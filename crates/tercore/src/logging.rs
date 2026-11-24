//! In-memory activity log shared by the AI, MCP and project subsystems.
//!
//! The bottom panel in the UI renders this ring buffer (MCP activity, AI
//! requests, diagnostics). It is bounded so a long session cannot grow without
//! limit, and it is broadcast so the UI stays live.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    App,
    Ai,
    Mcp,
    Studio,
    Project,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub id: u64,
    pub at: DateTime<Utc>,
    pub level: LogLevel,
    pub source: LogSource,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

const DEFAULT_CAPACITY: usize = 2_000;

/// Bounded, cloneable log buffer with a broadcast channel for live subscribers.
#[derive(Clone)]
pub struct ActivityLog {
    inner: Arc<Inner>,
}

struct Inner {
    entries: Mutex<VecDeque<LogEntry>>,
    next_id: Mutex<u64>,
    capacity: usize,
    tx: broadcast::Sender<LogEntry>,
}

impl ActivityLog {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(VecDeque::with_capacity(capacity.min(256))),
                next_id: Mutex::new(1),
                capacity,
                tx,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.inner.tx.subscribe()
    }

    pub fn push(&self, level: LogLevel, source: LogSource, message: impl Into<String>) {
        self.push_detailed(level, source, message, None);
    }

    pub fn push_detailed(
        &self,
        level: LogLevel,
        source: LogSource,
        message: impl Into<String>,
        detail: Option<String>,
    ) {
        let id = {
            let mut next = self.inner.next_id.lock().expect("log id lock");
            let id = *next;
            *next += 1;
            id
        };
        let entry = LogEntry {
            id,
            at: Utc::now(),
            level,
            source,
            message: message.into(),
            detail,
        };
        {
            let mut entries = self.inner.entries.lock().expect("log buffer lock");
            if entries.len() == self.inner.capacity {
                entries.pop_front();
            }
            entries.push_back(entry.clone());
        }
        // A send error only means nobody is listening right now.
        let _ = self.inner.tx.send(entry);
    }

    pub fn info(&self, source: LogSource, message: impl Into<String>) {
        self.push(LogLevel::Info, source, message);
    }

    pub fn warn(&self, source: LogSource, message: impl Into<String>) {
        self.push(LogLevel::Warn, source, message);
    }

    pub fn error(&self, source: LogSource, message: impl Into<String>) {
        self.push(LogLevel::Error, source, message);
    }

    pub fn debug(&self, source: LogSource, message: impl Into<String>) {
        self.push(LogLevel::Debug, source, message);
    }

    /// Most recent entries, oldest first.
    pub fn snapshot(&self, limit: usize) -> Vec<LogEntry> {
        let entries = self.inner.entries.lock().expect("log buffer lock");
        let skip = entries.len().saturating_sub(limit);
        entries.iter().skip(skip).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.entries.lock().expect("log buffer lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.inner
            .entries
            .lock()
            .expect("log buffer lock")
            .clear();
    }
}

impl Default for ActivityLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_capacity() {
        let log = ActivityLog::with_capacity(3);
        for i in 0..5 {
            log.info(LogSource::App, format!("line {i}"));
        }
        let snap = log.snapshot(10);
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].message, "line 2");
        assert_eq!(snap[2].message, "line 4");
    }

    #[tokio::test]
    async fn broadcasts_to_subscribers() {
        let log = ActivityLog::new();
        let mut rx = log.subscribe();
        log.error(LogSource::Mcp, "connection lost");
        let entry = rx.recv().await.expect("entry");
        assert_eq!(entry.level, LogLevel::Error);
        assert_eq!(entry.source, LogSource::Mcp);
    }

    #[test]
    fn ids_are_monotonic() {
        let log = ActivityLog::new();
        log.info(LogSource::App, "a");
        log.info(LogSource::App, "b");
        let snap = log.snapshot(2);
        assert_eq!(snap[1].id, snap[0].id + 1);
    }
}
