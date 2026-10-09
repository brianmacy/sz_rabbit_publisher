use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use sz_rabbit_publisher::{PublisherConfig, RabbitMQPublisher, Stats};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Version reported by `--version`. Release builds set `RELEASE_VERSION` from
/// the git tag (`v0.6.1` -> `0.6.1`) in `.github/workflows/release.yml`; other
/// builds fall back to the `Cargo.toml` version.
const VERSION: &str = match option_env!("RELEASE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser, Debug)]
#[command(
    name = "sz_rabbit_publisher",
    version = VERSION,
    about = "High-performance RabbitMQ publisher for JSONL files",
    long_about = None,
    after_help = "\
Multiple files are processed sequentially by default. Use --parallel to
publish all files concurrently (one AMQP connection per file). Each file
prints its own summary; an overall summary is shown when processing
multiple files.

Progress output fields:
  total      Lines read from input file
  acked      Messages confirmed by broker AND routed to a queue
  nacked     Broker rejections (each retry counts; retried forever)
  returned   Unroutable returns (no bound queue; retried forever, never acked)
  republished  Re-sent after a connection failure (possible duplicates)
  pending    Messages published but not yet confirmed
  throttled  Times the reader blocked waiting for publish capacity
  rate       Confirmed messages per second (interval rate, not cumulative)

Exit status is non-zero unless every record read was confirmed. On SIGINT/
SIGTERM the summary prints the exact --skip-lines resume point."
)]
struct Args {
    /// One or more JSONL files (plain text, gzip, or bzip2 — auto-detected)
    #[arg(value_name = "INPUT_FILE", required = true, num_args = 1..)]
    input_files: Vec<PathBuf>,

    /// RabbitMQ connection URL
    #[arg(
        short = 'u',
        long = "url",
        env = "RABBITMQ_URL",
        default_value = "amqp://guest:guest@localhost:5672/%2F"
    )]
    amqp_url: String,

    /// Exchange name
    #[arg(
        short = 'e',
        long = "exchange",
        env = "RABBITMQ_EXCHANGE",
        default_value = "senzing-rabbitmq-exchange"
    )]
    exchange: String,

    /// Queue name
    #[arg(
        short = 'q',
        long = "queue",
        env = "RABBITMQ_QUEUE",
        default_value = "senzing-rabbitmq-queue"
    )]
    queue: String,

    /// Routing key
    #[arg(
        short = 'r',
        long = "routing-key",
        env = "RABBITMQ_ROUTING_KEY",
        default_value = "senzing.records"
    )]
    routing_key: String,

    /// Max pending confirmations
    #[arg(short = 'm', long = "max-pending", default_value = "500")]
    max_pending: usize,

    /// Progress report interval (messages)
    #[arg(long = "report-interval", default_value = "10000")]
    report_interval: u64,

    /// Retry delay on nack (seconds)
    #[arg(long = "retry-delay", default_value = "3")]
    retry_delay: u64,

    /// Skip the first N non-empty records WITHOUT publishing them, to resume an
    /// interrupted load. Single-file input only. Compressed inputs have no seek,
    /// so the skipped prefix is decoded and discarded.
    /// WARNING: skipped records are never sent by this run. Over-skipping LOSES
    /// every record between the true resume point and N. Only pass a count that
    /// a previous run reported as confirmed (its "resume with --skip-lines N"
    /// message); under-skipping is the safe direction (re-sent records are
    /// duplicates, which the engine's idempotent add_record absorbs).
    #[arg(long = "skip-lines", env = "SENZING_SKIP_LINES", default_value = "0")]
    skip_lines: u64,

    /// Process files in parallel (one connection per file)
    #[arg(short = 'p', long = "parallel")]
    parallel: bool,

    /// Enable verbose logging
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing
    let log_level = if args.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_level(true),
        )
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level.to_string())),
        )
        .init();

    // Validate all input files exist up front
    for path in &args.input_files {
        if !path.exists() {
            anyhow::bail!("Input file does not exist: {}", path.display());
        }
    }

    // --skip-lines is a single-file resume knob; applying it per-file across many
    // files would silently drop the first N records of EACH file. Reject loudly.
    if args.skip_lines > 0 && args.input_files.len() > 1 {
        anyhow::bail!(
            "--skip-lines is only valid with a single input file (got {})",
            args.input_files.len()
        );
    }

    let config = PublisherConfig {
        amqp_url: args.amqp_url,
        exchange: args.exchange,
        queue: args.queue,
        routing_key: args.routing_key,
        max_pending: args.max_pending,
        retry_delay: Duration::from_secs(args.retry_delay),
        report_interval: args.report_interval,
        skip_lines: args.skip_lines,
    };

    let multi_file = args.input_files.len() > 1;
    // One publisher per file, created up front so a stop signal can report
    // exactly what each one confirmed.
    let mut jobs: Vec<(PathBuf, Arc<RabbitMQPublisher>)> = Vec::new();
    for path in args.input_files {
        let publisher = RabbitMQPublisher::new(config.clone());
        if multi_file {
            let label = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| path.display().to_string());
            publisher.stats_label(&label);
        }
        jobs.push((path, Arc::new(publisher)));
    }

    let file_stats = tokio::select! {
        res = run_jobs(&jobs, args.parallel) => res?,
        (name, code) = stop_signal() => {
            report_interrupted(&jobs, name);
            std::process::exit(code);
        }
    };

    // Overall summary when multiple files were processed
    if file_stats.len() > 1 {
        println!("\n=== Overall Summary ({} files) ===", file_stats.len());
        println!("{}", merge_all(&file_stats).final_summary());
    }

    Ok(())
}

async fn publish_one_file(path: &Path, publisher: &RabbitMQPublisher) -> Result<Stats> {
    let path_str = path.to_str().context("Invalid file path encoding")?;
    publisher
        .publish_file(path_str)
        .await
        .with_context(|| format!("Failed to publish file: {}", path.display()))
}

fn merge_all(stats: &[Stats]) -> Stats {
    stats
        .iter()
        .skip(1)
        .fold(stats[0].clone(), |acc, s| acc.merge(s))
}

/// Publish every file (sequentially, or concurrently with `--parallel`).
async fn run_jobs(
    jobs: &[(PathBuf, Arc<RabbitMQPublisher>)],
    parallel: bool,
) -> Result<Vec<Stats>> {
    if !parallel {
        let mut results = Vec::new();
        for (path, publisher) in jobs {
            results.push(publish_one_file(path, publisher).await?);
        }
        return Ok(results);
    }

    // One task per file, each with its own AMQP connection
    let mut join_set = tokio::task::JoinSet::new();
    for (path, publisher) in jobs {
        let (path, publisher) = (path.clone(), publisher.clone());
        join_set.spawn(async move { publish_one_file(&path, &publisher).await });
    }
    let mut results = Vec::new();
    let mut first_error: Option<anyhow::Error> = None;
    while let Some(res) = join_set.join_next().await {
        match res.context("Publisher task panicked").and_then(|r| r) {
            Ok(stats) => results.push(stats),
            Err(e) => {
                tracing::error!("{:#}", e);
                first_error.get_or_insert(e);
            }
        }
    }
    // Print partial summary before propagating the error
    if let Some(err) = first_error {
        if !results.is_empty() {
            println!(
                "\n=== Partial Summary ({} of {} files completed) ===",
                results.len(),
                jobs.len()
            );
            println!("{}", merge_all(&results).final_summary());
        }
        return Err(err);
    }
    Ok(results)
}

/// Resolves on SIGINT (Ctrl-C) or, on Unix, SIGTERM; yields (name, exit code).
async fn stop_signal() -> (&'static str, i32) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = tokio::signal::ctrl_c() => ("SIGINT", 130),
                _ = term.recv() => ("SIGTERM", 143),
            },
            Err(e) => {
                tracing::warn!("Cannot install SIGTERM handler: {e}");
                let _ = tokio::signal::ctrl_c().await;
                ("SIGINT", 130)
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        ("SIGINT", 130)
    }
}

/// On a stop signal: print each file's stats and the exact resume point.
fn report_interrupted(jobs: &[(PathBuf, Arc<RabbitMQPublisher>)], signal: &str) {
    eprintln!("\n=== INTERRUPTED by {signal}: publishing did NOT complete ===");
    let multi_file = jobs.len() > 1;
    for (path, publisher) in jobs {
        let stats = publisher.stats();
        println!("\n[{}] {}", path.display(), stats.final_summary());
        println!(
            "{}",
            interrupted_line(&stats, publisher.skip_lines(), multi_file)
        );
    }
}

/// The per-file stop report. `--skip-lines` is rejected with several input
/// files, so in multi-file mode the hint says to resume the file on its own.
fn interrupted_line(stats: &Stats, skip_lines: u64, multi_file: bool) -> String {
    if stats.total_records == 0 {
        return "Not started: re-run this file from the beginning".to_string();
    }
    let how = if multi_file {
        "--skip-lines is single-file only: resume by running this file ON ITS OWN with"
    } else {
        "Resume this file with"
    };
    format!(
        "INTERRUPTED: {} of {} records read were confirmed; the first {} are all confirmed. \
         {how} --skip-lines {}",
        stats.acked,
        stats.total_records,
        stats.confirmed_prefix,
        skip_lines + stats.confirmed_prefix
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_single_file_arg() {
        let args = Args::try_parse_from(["sz_rabbit_publisher", "file.jsonl"]).unwrap();
        assert_eq!(args.input_files, vec![PathBuf::from("file.jsonl")]);
        assert!(!args.parallel);
    }

    #[test]
    fn test_multiple_file_args() {
        let args =
            Args::try_parse_from(["sz_rabbit_publisher", "a.jsonl", "b.jsonl", "c.jsonl"]).unwrap();
        assert_eq!(args.input_files.len(), 3);
        assert!(!args.parallel);
    }

    #[test]
    fn test_parallel_flag() {
        let args =
            Args::try_parse_from(["sz_rabbit_publisher", "--parallel", "a.jsonl", "b.jsonl"])
                .unwrap();
        assert!(args.parallel);
        assert_eq!(args.input_files.len(), 2);
    }

    #[test]
    fn test_parallel_short_flag() {
        let args = Args::try_parse_from(["sz_rabbit_publisher", "-p", "a.jsonl"]).unwrap();
        assert!(args.parallel);
    }

    #[test]
    fn test_no_files_fails() {
        let result = Args::try_parse_from(["sz_rabbit_publisher"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_skip_lines_default_zero() {
        let args = Args::try_parse_from(["sz_rabbit_publisher", "file.jsonl"]).unwrap();
        assert_eq!(args.skip_lines, 0);
    }

    #[test]
    fn test_skip_lines_parsed() {
        let args = Args::try_parse_from([
            "sz_rabbit_publisher",
            "--skip-lines",
            "6420000",
            "file.jsonl",
        ])
        .unwrap();
        assert_eq!(args.skip_lines, 6_420_000);
    }

    #[test]
    fn test_interrupted_line_reports_resume_point() {
        let mut stats = Stats::default();
        stats.total_records = 100;
        stats.acked = 90;
        stats.confirmed_prefix = 85;
        let line = interrupted_line(&stats, 1000, false);
        assert!(line.contains("90 of 100"), "{line}");
        assert!(
            line.contains("Resume this file with --skip-lines 1085"),
            "{line}"
        );
        assert!(interrupted_line(&Stats::default(), 0, false).contains("Not started"));
    }

    #[test]
    fn test_interrupted_line_multi_file_says_run_file_alone() {
        let mut stats = Stats::default();
        stats.total_records = 100;
        stats.confirmed_prefix = 85;
        let line = interrupted_line(&stats, 0, true);
        assert!(!line.contains("Resume this file with"), "{line}");
        assert!(line.contains("ON ITS OWN with --skip-lines 85"), "{line}");
    }
}
