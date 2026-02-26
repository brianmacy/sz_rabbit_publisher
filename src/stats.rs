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

    pub fn progress_report(&self) -> String {
        format!(
            "Progress: total={}, acked={}, nacked={}, pending={}, throttled={}, rate={:.2} msg/s",
            self.total_records,
            self.acked,
            self.nacked,
            self.pending,
            self.throttled,
            self.messages_per_second()
        )
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
}

impl StatsTracker {
    pub fn new(report_interval: u64) -> Self {
        Self {
            stats: Arc::new(Mutex::new(Stats::new())),
            report_interval,
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
            tracing::info!("{}", stats.progress_report());
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
        let stats = self.stats.lock().unwrap();
        tracing::info!("{}", stats.progress_report());
    }

    pub fn print_final_summary(&self) {
        let stats = self.stats.lock().unwrap();
        println!("\n{}", stats.final_summary());
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
        let stats = Stats {
            total_records: 100,
            acked: 95,
            nacked: 5,
            pending: 10,
            throttled: 2,
            start_time: Some(Instant::now()),
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
        };

        let summary = stats.final_summary();
        assert!(summary.contains("Total records: 1000"));
        assert!(summary.contains("Acknowledged: 990"));
        assert!(summary.contains("Not acknowledged: 10"));
        assert!(summary.contains("Throttled: 5"));
    }
}
