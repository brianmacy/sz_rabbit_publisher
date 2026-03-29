use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use sz_rabbit_publisher::{PublisherConfig, RabbitMQPublisher, Stats};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(
    name = "sz_rabbit_publisher",
    version = "0.1.0",
    about = "High-performance RabbitMQ publisher for JSONL files",
    long_about = None,
    after_help = "\
Multiple files are processed sequentially by default. Use --parallel to
publish all files concurrently (one AMQP connection per file). Each file
prints its own summary; an overall summary is shown when processing
multiple files.

Progress output fields:
  total      Lines read from input file
  acked      Messages confirmed by broker (only genuine acks count)
  nacked     Broker rejections (messages are retried forever)
  pending    Messages published but not yet confirmed
  throttled  Times the reader blocked waiting for publish capacity
  rate       Confirmed messages per second (interval rate, not cumulative)"
)]
struct Args {
    /// One or more JSONL files (plain text or gzip)
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

    let config = PublisherConfig {
        amqp_url: args.amqp_url,
        exchange: args.exchange,
        queue: args.queue,
        routing_key: args.routing_key,
        max_pending: args.max_pending,
        retry_delay: Duration::from_secs(args.retry_delay),
        report_interval: args.report_interval,
    };

    let multi_file = args.input_files.len() > 1;

    let file_stats: Vec<Stats> = if args.parallel {
        // One task per file, each with its own publisher and AMQP connection
        let mut join_set = tokio::task::JoinSet::new();
        for path in args.input_files {
            let cfg = config.clone();
            join_set.spawn(async move {
                let path_str = path.to_str().context("Invalid file path encoding")?;
                let publisher = RabbitMQPublisher::new(cfg);
                if multi_file {
                    let label = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(path_str);
                    publisher.stats_label(label);
                }
                publisher
                    .publish_file(path_str)
                    .await
                    .with_context(|| format!("Failed to publish file: {}", path.display()))
            });
        }
        let mut results = Vec::new();
        let mut first_error: Option<anyhow::Error> = None;
        while let Some(res) = join_set.join_next().await {
            match res.context("Publisher task panicked").and_then(|r| r) {
                Ok(stats) => results.push(stats),
                Err(e) => {
                    tracing::error!("{:#}", e);
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        // Print partial summary before propagating the error
        if let Some(err) = first_error {
            if !results.is_empty() {
                let overall = results
                    .iter()
                    .skip(1)
                    .fold(results[0].clone(), |acc, s| acc.merge(s));
                println!(
                    "\n=== Partial Summary ({} of {} files completed) ===",
                    results.len(),
                    results.len() + 1
                );
                println!("{}", overall.final_summary());
            }
            return Err(err);
        }
        results
    } else {
        // Sequential: fresh publisher per file for clean per-file stats
        let mut results = Vec::new();
        for path in &args.input_files {
            let path_str = path.to_str().context("Invalid file path encoding")?;
            let publisher = RabbitMQPublisher::new(config.clone());
            if multi_file {
                let label = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path_str);
                publisher.stats_label(label);
            }
            let stats = publisher
                .publish_file(path_str)
                .await
                .with_context(|| format!("Failed to publish file: {}", path.display()))?;
            results.push(stats);
        }
        results
    };

    // Overall summary when multiple files were processed
    if file_stats.len() > 1 {
        let overall = file_stats
            .iter()
            .skip(1)
            .fold(file_stats[0].clone(), |acc, s| acc.merge(s));
        println!("\n=== Overall Summary ({} files) ===", file_stats.len());
        println!("{}", overall.final_summary());
    }

    Ok(())
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
}
