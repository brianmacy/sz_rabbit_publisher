use anyhow::Result;
use lapin::{Connection, ConnectionProperties, options::*, types::FieldTable};
use std::io::Write;
use std::time::Duration;
use sz_rabbit_publisher::{PublisherConfig, RabbitMQPublisher};
use tempfile::NamedTempFile;

/// Helper to create a test JSONL file
fn create_test_jsonl_file(lines: &[&str]) -> Result<NamedTempFile> {
    let mut temp_file = NamedTempFile::new()?;
    for line in lines {
        writeln!(temp_file, "{}", line)?;
    }
    temp_file.flush()?;
    Ok(temp_file)
}

/// Helper to create a gzip-compressed JSONL file
fn create_test_gzip_file(lines: &[&str]) -> Result<NamedTempFile> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let mut temp_file = NamedTempFile::new()?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());

    for line in lines {
        writeln!(encoder, "{}", line)?;
    }

    let compressed = encoder.finish()?;
    temp_file.write_all(&compressed)?;
    temp_file.flush()?;
    Ok(temp_file)
}

/// Get RabbitMQ URL from environment or use default
fn get_rabbitmq_url() -> String {
    std::env::var("TEST_RABBITMQ_URL")
        .unwrap_or_else(|_| "amqp://guest:guest@127.0.0.1:5672/%2F".to_string())
}

/// Helper to set up RabbitMQ infrastructure (exchange, queue, binding)
async fn setup_rabbitmq(
    amqp_url: &str,
    exchange: &str,
    queue: &str,
    routing_key: &str,
) -> Result<()> {
    let connection = Connection::connect(amqp_url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;

    // Declare exchange
    channel
        .exchange_declare(
            exchange,
            lapin::ExchangeKind::Direct,
            ExchangeDeclareOptions {
                durable: false,
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    // Declare queue
    channel
        .queue_declare(
            queue,
            QueueDeclareOptions {
                durable: false,
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    // Bind queue to exchange
    channel
        .queue_bind(
            queue,
            exchange,
            routing_key,
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    connection.close(0, "Setup complete").await.ok();
    Ok(())
}

#[tokio::test]
async fn test_publish_small_file() -> Result<()> {
    let amqp_url = get_rabbitmq_url();

    // Set up RabbitMQ infrastructure
    setup_rabbitmq(&amqp_url, "test-exchange", "test-queue", "test.key").await?;

    let test_data = vec![
        r#"{"id": 1, "name": "Alice"}"#,
        r#"{"id": 2, "name": "Bob"}"#,
        r#"{"id": 3, "name": "Charlie"}"#,
    ];

    let temp_file = create_test_jsonl_file(&test_data)?;

    let config = PublisherConfig {
        amqp_url,
        exchange: "test-exchange".to_string(),
        queue: "test-queue".to_string(),
        routing_key: "test.key".to_string(),
        max_pending: 100,
        retry_delay: Duration::from_secs(1),
        report_interval: 1,
    };

    let publisher = RabbitMQPublisher::new(config);

    let stats = publisher
        .publish_file(temp_file.path().to_str().unwrap())
        .await?;

    // Verify all messages were published
    assert_eq!(stats.total_records, 3);
    assert_eq!(stats.acked, 3);
    assert_eq!(stats.nacked, 0);

    Ok(())
}

#[tokio::test]
async fn test_publish_gzip_file() -> Result<()> {
    let amqp_url = get_rabbitmq_url();

    // Set up RabbitMQ infrastructure
    setup_rabbitmq(
        &amqp_url,
        "test-exchange-gz",
        "test-queue-gz",
        "test.key.gz",
    )
    .await?;

    let test_data = vec![
        r#"{"id": 1, "name": "Alice"}"#,
        r#"{"id": 2, "name": "Bob"}"#,
        r#"{"id": 3, "name": "Charlie"}"#,
    ];

    let temp_file = create_test_gzip_file(&test_data)?;

    let config = PublisherConfig {
        amqp_url,
        exchange: "test-exchange-gz".to_string(),
        queue: "test-queue-gz".to_string(),
        routing_key: "test.key.gz".to_string(),
        max_pending: 100,
        retry_delay: Duration::from_secs(1),
        report_interval: 1,
    };

    let publisher = RabbitMQPublisher::new(config);

    let stats = publisher
        .publish_file(temp_file.path().to_str().unwrap())
        .await?;

    // Verify all messages were published
    assert_eq!(stats.total_records, 3);
    assert_eq!(stats.acked, 3);
    assert_eq!(stats.nacked, 0);

    Ok(())
}

#[tokio::test]
async fn test_publish_empty_file() -> Result<()> {
    let amqp_url = get_rabbitmq_url();

    // Set up RabbitMQ infrastructure
    setup_rabbitmq(
        &amqp_url,
        "test-exchange-empty",
        "test-queue-empty",
        "test.key.empty",
    )
    .await?;

    let temp_file = create_test_jsonl_file(&[])?;

    let config = PublisherConfig {
        amqp_url,
        exchange: "test-exchange-empty".to_string(),
        queue: "test-queue-empty".to_string(),
        routing_key: "test.key.empty".to_string(),
        max_pending: 100,
        retry_delay: Duration::from_secs(1),
        report_interval: 1,
    };

    let publisher = RabbitMQPublisher::new(config);

    let stats = publisher
        .publish_file(temp_file.path().to_str().unwrap())
        .await?;

    // Verify no messages for empty file
    assert_eq!(stats.total_records, 0);
    assert_eq!(stats.acked, 0);

    Ok(())
}

#[tokio::test]
async fn test_publish_large_file_with_throttling() -> Result<()> {
    let amqp_url = get_rabbitmq_url();

    // Set up RabbitMQ infrastructure
    setup_rabbitmq(
        &amqp_url,
        "test-exchange-large",
        "test-queue-large",
        "test.key.large",
    )
    .await?;

    // Create a file with more messages than max_pending
    let test_data: Vec<String> = (0..1000)
        .map(|i| format!(r#"{{"id": {}, "name": "User{}}}"#, i, i))
        .collect();

    let test_data_refs: Vec<&str> = test_data.iter().map(|s| s.as_str()).collect();
    let temp_file = create_test_jsonl_file(&test_data_refs)?;

    let config = PublisherConfig {
        amqp_url,
        exchange: "test-exchange-large".to_string(),
        queue: "test-queue-large".to_string(),
        routing_key: "test.key.large".to_string(),
        max_pending: 50, // Small max_pending to test throttling
        retry_delay: Duration::from_millis(100),
        report_interval: 100,
    };

    let publisher = RabbitMQPublisher::new(config);

    let stats = publisher
        .publish_file(temp_file.path().to_str().unwrap())
        .await?;

    // Verify all messages were published
    assert_eq!(stats.total_records, 1000, "Should have read 1000 records");
    assert_eq!(stats.acked, 1000, "All 1000 messages should be acked");
    assert_eq!(stats.nacked, 0, "No messages should be nacked");

    // CRITICAL: Verify back pressure actually occurred!
    // With 1000 messages and max_pending=50, we MUST have throttling
    assert!(
        stats.throttled > 0,
        "Back pressure FAILED: throttled={}, but should be >0 with 1000 messages and max_pending=50",
        stats.throttled
    );

    println!(
        "✓ Back pressure verified: {} throttle events with max_pending=50",
        stats.throttled
    );

    Ok(())
}

#[tokio::test]
async fn test_invalid_file_path() {
    let amqp_url = get_rabbitmq_url();

    let config = PublisherConfig {
        amqp_url,
        exchange: "test-exchange".to_string(),
        queue: "test-queue".to_string(),
        routing_key: "test.key".to_string(),
        max_pending: 100,
        retry_delay: Duration::from_secs(1),
        report_interval: 1,
    };

    let publisher = RabbitMQPublisher::new(config);

    let result = publisher.publish_file("/nonexistent/file.jsonl").await;

    // Test should fail with non-existent file
    assert!(result.is_err(), "Expected error for non-existent file");
}
