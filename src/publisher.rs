use anyhow::{Context, Result};
use lapin::{options::*, BasicProperties, Channel, Connection, ConnectionProperties};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::file_reader::FileReader;
use crate::stats::StatsTracker;

/// Configuration for the RabbitMQ publisher
#[derive(Debug, Clone)]
pub struct PublisherConfig {
    pub amqp_url: String,
    pub exchange: String,
    pub queue: String,
    pub routing_key: String,
    pub max_pending: usize,
    pub retry_delay: Duration,
    pub report_interval: u64,
}

/// RabbitMQ publisher with delivery confirmations and back pressure
pub struct RabbitMQPublisher {
    config: PublisherConfig,
    stats: StatsTracker,
}

impl RabbitMQPublisher {
    pub fn new(config: PublisherConfig) -> Self {
        let stats = StatsTracker::new(config.report_interval);
        Self { config, stats }
    }

    /// Publishes all lines from a file to RabbitMQ
    /// Returns statistics about the publishing operation
    pub async fn publish_file(&self, file_path: &str) -> Result<crate::stats::Stats> {
        tracing::info!("Opening file: {}", file_path);
        let mut reader = FileReader::open(file_path)
            .await
            .context("Failed to open input file")?;

        tracing::info!("Total lines to publish: {}", reader.total_lines());

        // Connect to RabbitMQ
        tracing::info!("Connecting to RabbitMQ at {}", self.config.amqp_url);
        let connection =
            Connection::connect(&self.config.amqp_url, ConnectionProperties::default())
                .await
                .context("Failed to connect to RabbitMQ")?;

        let channel = connection
            .create_channel()
            .await
            .context("Failed to create channel")?;

        // Enable publisher confirms
        channel
            .confirm_select(ConfirmSelectOptions::default())
            .await
            .context("Failed to enable publisher confirms")?;

        tracing::info!(
            "Publishing to exchange: {}, queue: {}, routing_key: {}",
            self.config.exchange,
            self.config.queue,
            self.config.routing_key
        );

        // Set up bounded channel for back pressure
        let (tx, mut rx) = mpsc::channel::<String>(self.config.max_pending);

        // Spawn file reader task
        let reader_handle = {
            let stats = self.stats.clone();
            tokio::spawn(async move {
                while let Some(line) = reader.next_line() {
                    stats.increment_total();

                    // Try non-blocking send first to detect back pressure
                    match tx.try_send(line.clone()) {
                        Ok(_) => {
                            // Sent without blocking
                        }
                        Err(mpsc::error::TrySendError::Full(msg)) => {
                            // Channel is full - back pressure!
                            stats.increment_throttled();
                            // Now do blocking send
                            if tx.send(msg).await.is_err() {
                                break; // Channel closed
                            }
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            break; // Channel closed
                        }
                    }
                }
            })
        };

        // Publish messages as they become available
        while let Some(message) = rx.recv().await {
            self.publish_with_retry(&channel, &message).await?;
        }

        // Wait for reader to complete
        reader_handle.await.context("File reader task failed")?;

        // Close connection gracefully
        connection
            .close(0, "Publishing complete")
            .await
            .context("Failed to close connection")?;

        self.stats.print_final_summary();

        Ok(self.stats.get_snapshot())
    }

    /// Publishes a single message with retry on nack
    async fn publish_with_retry(&self, channel: &Channel, message: &str) -> Result<()> {
        let mut retry_count = 0;
        const MAX_RETRIES: u32 = 3;

        loop {
            self.stats.increment_pending();

            let confirmation = channel
                .basic_publish(
                    &self.config.exchange,
                    &self.config.routing_key,
                    BasicPublishOptions::default(),
                    message.as_bytes(),
                    BasicProperties::default().with_delivery_mode(2), // Persistent
                )
                .await
                .context("Failed to publish message")?
                .await
                .context("Failed to get confirmation")?;

            if confirmation.is_ack() {
                self.stats.increment_acked();
                return Ok(());
            } else if confirmation.is_nack() {
                self.stats.increment_nacked();
                retry_count += 1;

                if retry_count >= MAX_RETRIES {
                    anyhow::bail!("Message nacked after {} retries", MAX_RETRIES);
                }

                tracing::warn!(
                    "Message nacked (attempt {}/{}), will retry after {:?}",
                    retry_count,
                    MAX_RETRIES,
                    self.config.retry_delay
                );

                sleep(self.config.retry_delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publisher_config_creation() {
        let config = PublisherConfig {
            amqp_url: "amqp://localhost".to_string(),
            exchange: "test-exchange".to_string(),
            queue: "test-queue".to_string(),
            routing_key: "test.key".to_string(),
            max_pending: 500,
            retry_delay: Duration::from_secs(3),
            report_interval: 10000,
        };

        assert_eq!(config.amqp_url, "amqp://localhost");
        assert_eq!(config.exchange, "test-exchange");
        assert_eq!(config.max_pending, 500);
    }

    #[test]
    fn test_publisher_creation() {
        let config = PublisherConfig {
            amqp_url: "amqp://localhost".to_string(),
            exchange: "test-exchange".to_string(),
            queue: "test-queue".to_string(),
            routing_key: "test.key".to_string(),
            max_pending: 500,
            retry_delay: Duration::from_secs(3),
            report_interval: 10000,
        };

        let publisher = RabbitMQPublisher::new(config);
        assert_eq!(publisher.config.amqp_url, "amqp://localhost");
    }
}
