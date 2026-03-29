use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Statistics for tracking message publishing progress
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_records: u64,
    pub acked: u64,
    pub nacked: u64,
    pub throttled: u64,
    pub pending: u64,
    pub start_time: Option<Instant>,
    /// Tracks the last progress report for interval rate calculation
    last_report_time: Option<Instant>,
    last_report_acked: u64,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            start_time: Some(Instant::now()),
            ..Default::default()
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time
            .map_or(Duration::ZERO, |start| start.elapsed())
    }

    pub fn messages_per_second(&self) -> f64 {
        let elapsed = self.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.acked as f64 / elapsed
        } else {
            0.0
        }
    }

    /// Rate since the last progress report (or since start for the first report)
    fn interval_rate(&self) -> f64 {
        let interval_elapsed = self
            .last_report_time
            .unwrap_or_else(|| self.start_time.unwrap_or_else(Instant::now))
            .elapsed()
            .as_secs_f64();
        let interval_acked = self.acked.saturating_sub(self.last_report_acked);
        if interval_elapsed > 0.0 {
            interval_acked as f64 / interval_elapsed
        } else {
            0.0
        }
    }

    pub fn progress_report(&mut self) -> String {
        let rate = self.interval_rate();
        let report = format!(
            "Progress: total={}, acked={}, nacked={}, pending={}, throttled={}, rate={:.2} msg/s",
            self.total_records, self.acked, self.nacked, self.pending, self.throttled, rate
        );
        self.last_report_time = Some(Instant::now());
        self.last_report_acked = self.acked;
        report
    }

    /// Combine two Stats snapshots (for multi-file overall summary).
    /// Keeps the earliest start_time so elapsed covers the full wall-clock window.
    pub fn merge(&self, other: &Stats) -> Stats {
        Stats {
            total_records: self.total_records + other.total_records,
            acked: self.acked + other.acked,
            nacked: self.nacked + other.nacked,
            throttled: self.throttled + other.throttled,
            pending: self.pending + other.pending,
            start_time: match (self.start_time, other.start_time) {
                (Some(a), Some(b)) => Some(if a < b { a } else { b }),
                (a, None) => a,
                (None, b) => b,
            },
            last_report_time: None,
            last_report_acked: 0,
        }
    }

    pub fn final_summary(&self) -> String {
        let elapsed = self.elapsed();
        format!(
            "Final Summary:\n\
             Total records: {}\n\
             Acknowledged: {}\n\
             Not acknowledged: {}\n\
             Throttled: {}\n\
             Pending: {}\n\
             Elapsed time: {:.2}s\n\
             Average rate: {:.2} msg/s",
            self.total_records,
            self.acked,
            self.nacked,
            self.throttled,
            self.pending,
            elapsed.as_secs_f64(),
            self.messages_per_second()
        )
    }
}

/// Thread-safe statistics tracker
#[derive(Debug, Clone)]
pub struct StatsTracker {
    stats: Arc<Mutex<Stats>>,
    report_interval: u64,
    label: Arc<Mutex<String>>,
}

impl StatsTracker {
    pub fn new(report_interval: u64) -> Self {
        Self {
            stats: Arc::new(Mutex::new(Stats::new())),
            report_interval,
            label: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Set a label (e.g. filename) that prefixes all progress and summary output
    pub fn set_label(&self, label: &str) {
        *self.label.lock().unwrap() = label.to_string();
    }

    fn format_label(&self) -> String {
        let label = self.label.lock().unwrap();
        if label.is_empty() {
            String::new()
        } else {
            format!("[{}] ", label)
        }
    }

    pub fn increment_total(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.total_records += 1;
    }

    pub fn increment_acked(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.acked += 1;
        if stats.pending > 0 {
            stats.pending -= 1;
        }

        // Report progress at acked intervals so rate reflects confirmed delivery
        if stats.acked.is_multiple_of(self.report_interval) {
            tracing::info!("{}{}", self.format_label(), stats.progress_report());
        }
    }

    pub fn increment_nacked(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.nacked += 1;
        if stats.pending > 0 {
            stats.pending -= 1;
        }
    }

    pub fn increment_throttled(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.throttled += 1;
    }

    pub fn increment_pending(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.pending += 1;
    }

    pub fn get_pending(&self) -> u64 {
        self.stats.lock().unwrap().pending
    }

    pub fn get_snapshot(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }

    pub fn print_progress(&self) {
        let mut stats = self.stats.lock().unwrap();
        tracing::info!("{}{}", self.format_label(), stats.progress_report());
    }

    pub fn print_final_summary(&self) {
        let label = self.format_label();
        let stats = self.stats.lock().unwrap();
        println!("\n{}{}", label, stats.final_summary());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_new() {
        let stats = Stats::new();
        assert_eq!(stats.total_records, 0);
        assert_eq!(stats.acked, 0);
        assert_eq!(stats.nacked, 0);
        assert_eq!(stats.throttled, 0);
        assert_eq!(stats.pending, 0);
        assert!(stats.start_time.is_some());
    }

    #[test]
    fn test_stats_tracker_increment() {
        let tracker = StatsTracker::new(10000);

        tracker.increment_total();
        tracker.increment_pending();
        tracker.increment_acked();

        let snapshot = tracker.get_snapshot();
        assert_eq!(snapshot.total_records, 1);
        assert_eq!(snapshot.acked, 1);
        assert_eq!(snapshot.pending, 0); // Incremented then decremented
    }

    #[test]
    fn test_stats_tracker_nacked() {
        let tracker = StatsTracker::new(10000);

        tracker.increment_total();
        tracker.increment_pending();
        tracker.increment_nacked();

        let snapshot = tracker.get_snapshot();
        assert_eq!(snapshot.total_records, 1);
        assert_eq!(snapshot.nacked, 1);
        assert_eq!(snapshot.pending, 0); // Incremented then decremented
    }

    #[test]
    fn test_stats_tracker_throttled() {
        let tracker = StatsTracker::new(10000);

        tracker.increment_throttled();
        tracker.increment_throttled();

        let snapshot = tracker.get_snapshot();
        assert_eq!(snapshot.throttled, 2);
    }

    #[test]
    fn test_messages_per_second() {
        let mut stats = Stats::new();
        std::thread::sleep(std::time::Duration::from_millis(100));
        stats.acked = 100;

        let rate = stats.messages_per_second();
        assert!(rate > 0.0);
        assert!(rate < 10000.0); // Should be around 1000 msg/s
    }

    #[test]
    fn test_progress_report_format() {
        let mut stats = Stats {
            total_records: 100,
            acked: 95,
            nacked: 5,
            pending: 10,
            throttled: 2,
            start_time: Some(Instant::now()),
            last_report_time: None,
            last_report_acked: 0,
        };

        let report = stats.progress_report();
        assert!(report.contains("total=100"));
        assert!(report.contains("acked=95"));
        assert!(report.contains("nacked=5"));
        assert!(report.contains("pending=10"));
        assert!(report.contains("throttled=2"));
    }

    #[test]
    fn test_final_summary_format() {
        let stats = Stats {
            total_records: 1000,
            acked: 990,
            nacked: 10,
            pending: 0,
            throttled: 5,
            start_time: Some(Instant::now()),
            last_report_time: None,
            last_report_acked: 0,
        };

        let summary = stats.final_summary();
        assert!(summary.contains("Total records: 1000"));
        assert!(summary.contains("Acknowledged: 990"));
        assert!(summary.contains("Not acknowledged: 10"));
        assert!(summary.contains("Throttled: 5"));
    }

    #[test]
    fn test_stats_merge() {
        let a = Stats {
            total_records: 100,
            acked: 90,
            nacked: 5,
            throttled: 2,
            pending: 0,
            start_time: Some(Instant::now()),
            last_report_time: None,
            last_report_acked: 0,
        };
        let b = Stats {
            total_records: 200,
            acked: 190,
            nacked: 8,
            throttled: 1,
            pending: 0,
            start_time: Some(Instant::now()),
            last_report_time: None,
            last_report_acked: 0,
        };
        let combined = a.merge(&b);
        assert_eq!(combined.total_records, 300);
        assert_eq!(combined.acked, 280);
        assert_eq!(combined.nacked, 13);
        assert_eq!(combined.throttled, 3);
        assert_eq!(combined.pending, 0);
        // Keeps the earliest start_time (a's, since Instant is monotonic)
        assert_eq!(combined.start_time, a.start_time);
    }

    #[test]
    fn test_interval_rate() {
        let mut stats = Stats {
            total_records: 0,
            acked: 0,
            nacked: 0,
            throttled: 0,
            pending: 0,
            start_time: Some(Instant::now()),
            last_report_time: None,
            last_report_acked: 0,
        };

        // First report: interval covers from start_time
        stats.acked = 1000;
        let report1 = stats.progress_report();
        assert!(report1.contains("acked=1000"));
        // After first report, last_report_acked should be updated
        assert_eq!(stats.last_report_acked, 1000);
        assert!(stats.last_report_time.is_some());

        // Second report: interval covers only new acks since last report
        std::thread::sleep(std::time::Duration::from_millis(50));
        stats.acked = 1500;
        let report2 = stats.progress_report();
        assert!(report2.contains("acked=1500"));
        assert_eq!(stats.last_report_acked, 1500);

        // The interval rate in report2 should reflect 500 acks over ~50ms
        // which is ~10000 msg/s, not 1500/total_elapsed (~750 msg/s cumulative)
        // Extract rate from report string
        let rate_str = report2.split("rate=").nth(1).unwrap();
        let rate: f64 = rate_str.trim_end_matches(" msg/s").parse().unwrap();
        // Interval rate should be > 5000 (500 acks in ~50ms), not cumulative ~750
        assert!(
            rate > 5000.0,
            "Interval rate {:.2} should be > 5000 (not cumulative average)",
            rate
        );
    }
}
