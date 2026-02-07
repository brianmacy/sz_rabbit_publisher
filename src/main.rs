use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use sz_rabbit_publisher::{PublisherConfig, RabbitMQPublisher};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(
    name = "sz_rabbit_publisher",
    version = "0.1.0",
    about = "High-performance RabbitMQ publisher for JSONL files",
    long_about = None
)]
struct Args {
    /// Path to JSONL file (plain text or gzip)
    #[arg(value_name = "INPUT_FILE")]
    input_file: PathBuf,

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

    // Validate input file exists
    if !args.input_file.exists() {
        anyhow::bail!("Input file does not exist: {}", args.input_file.display());
    }

    // Create publisher configuration
    let config = PublisherConfig {
        amqp_url: args.amqp_url,
        exchange: args.exchange,
        queue: args.queue,
        routing_key: args.routing_key,
        max_pending: args.max_pending,
        retry_delay: Duration::from_secs(args.retry_delay),
        report_interval: args.report_interval,
    };

    // Create and run publisher
    let publisher = RabbitMQPublisher::new(config);

    let input_file_str = args
        .input_file
        .to_str()
        .context("Invalid input file path")?;

    publisher
        .publish_file(input_file_str)
        .await
        .context("Failed to publish file")?;

    Ok(())
}
