use anyhow::{Context, Result};
use lapin::{
    BasicProperties, Channel, Connection, ConnectionProperties, PublisherConfirm, options::*,
    types::ShortString,
};
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

    /// Set a label (e.g. filename) that prefixes all progress and summary output
    pub fn stats_label(&self, label: &str) {
        self.stats.set_label(label);
    }

    /// Publishes all lines from a file to RabbitMQ
    /// Returns statistics about the publishing operation
    pub async fn publish_file(&self, file_path: &str) -> Result<crate::stats::Stats> {
        tracing::info!("Opening file: {}", file_path);
        let mut reader = FileReader::open(file_path)
            .await
            .context("Failed to open input file")?;

        // Connect to RabbitMQ (retries forever until connected)
        tracing::info!("Connecting to RabbitMQ at {}", self.config.amqp_url);
        let (mut connection, mut channel) = self.connect_with_confirms().await;

        tracing::info!(
            "Publishing to exchange: {}, queue: {}, routing_key: {}",
            self.config.exchange,
            self.config.queue,
            self.config.routing_key
        );

        // Buffer 2x the batch size so the reader can keep filling while confirms drain
        let (tx, mut rx) = mpsc::channel::<String>(self.config.max_pending * 2);

        // Spawn file reader task
        let reader_handle = {
            let stats = self.stats.clone();
            tokio::spawn(async move {
                loop {
                    let line = match reader.next_line() {
                        Some(Ok(line)) => line,
                        Some(Err(e)) => {
                            tracing::error!("Error reading line: {:#}", e);
                            break;
                        }
                        None => break, // EOF
                    };

                    stats.increment_total();

                    // Try non-blocking send first to detect back pressure
                    match tx.try_send(line) {
                        Ok(_) => {}
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
                tracing::info!("File reading complete: {} lines read", reader.lines_read());
            })
        };

        // Pipeline publishes: fire up to max_pending, collect confirms, then
        // await each confirm to verify actual ack/nack from the broker.
        // Nacked messages (reject-publish) are retried forever.
        // Connection drops trigger reconnection — no messages are lost.
        let mut pending: Vec<(String, PublisherConfirm)> =
            Vec::with_capacity(self.config.max_pending);
        let mut unsent: Vec<String> = Vec::new();
        let mut eof = false;

        while !eof || !unsent.is_empty() || !pending.is_empty() {
            // Fill batch: unsent (nacked/unconfirmed) messages first, then new from reader
            while pending.len() < self.config.max_pending {
                let message = if let Some(msg) = unsent.pop() {
                    msg
                } else if eof {
                    break;
                } else {
                    match rx.recv().await {
                        Some(msg) => msg,
                        None => {
                            eof = true;
                            break;
                        }
                    }
                };

                match channel
                    .basic_publish(
                        ShortString::from(self.config.exchange.as_str()),
                        ShortString::from(self.config.routing_key.as_str()),
                        BasicPublishOptions::default(),
                        message.as_bytes(),
                        BasicProperties::default().with_delivery_mode(2),
                    )
                    .await
                {
                    Ok(confirm) => {
                        self.stats.increment_pending();
                        pending.push((message, confirm));
                    }
                    Err(e) => {
                        tracing::warn!("Publish failed: {:#}, reconnecting...", e);
                        unsent.push(message);
                        for (msg, _) in pending.drain(..) {
                            unsent.push(msg);
                        }
                        (connection, channel) = self.reconnect(&connection).await;
                        break;
                    }
                }
            }

            // Drain confirms — check each for actual ack/nack
            let mut conn_failed = false;
            for (message, confirm) in pending.drain(..) {
                match confirm.await {
                    Ok(confirmation) => {
                        if confirmation.is_ack() {
                            self.stats.increment_acked();
                        } else {
                            self.stats.increment_nacked();
                            unsent.push(message);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Confirm failed: {:#}, reconnecting...", e);
                        unsent.push(message);
                        conn_failed = true;
                        break;
                    }
                }
            }

            if conn_failed {
                // Remaining pending items can't be confirmed — re-publish after reconnect
                for (msg, _) in pending.drain(..) {
                    unsent.push(msg);
                }
                (connection, channel) = self.reconnect(&connection).await;
            }

            // Back off before retrying nacked messages (not after reconnect — that already waited)
            if !unsent.is_empty() && !conn_failed {
                sleep(self.config.retry_delay).await;
            }
        }

        // Wait for reader to complete
        reader_handle.await.context("File reader task failed")?;

        // Close connection gracefully
        if let Err(e) = connection.close(0, "Publishing complete".into()).await {
            tracing::warn!("Failed to close connection gracefully: {:#}", e);
        }

        self.stats.print_final_summary();

        Ok(self.stats.get_snapshot())
    }

    /// Connect to RabbitMQ with publisher confirms enabled, retrying forever.
    async fn connect_with_confirms(&self) -> (Connection, Channel) {
        loop {
            match self.try_connect().await {
                Ok(result) => return result,
                Err(e) => {
                    tracing::warn!(
                        "RabbitMQ connection failed: {:#}, retrying in {:?}...",
                        e,
                        self.config.retry_delay
                    );
                    sleep(self.config.retry_delay).await;
                }
            }
        }
    }

    /// Single connection attempt: connect, create channel, enable confirms.
    async fn try_connect(&self) -> Result<(Connection, Channel)> {
        let conn = Connection::connect(&self.config.amqp_url, ConnectionProperties::default())
            .await
            .context("Failed to connect to RabbitMQ")?;
        let ch = conn
            .create_channel()
            .await
            .context("Failed to create channel")?;
        ch.confirm_select(ConfirmSelectOptions::default())
            .await
            .context("Failed to enable publisher confirms")?;
        Ok((conn, ch))
    }

    /// Close old connection (best-effort) and reconnect, retrying forever.
    async fn reconnect(&self, old_conn: &Connection) -> (Connection, Channel) {
        if let Err(e) = old_conn.close(0, "reconnecting".into()).await {
            tracing::debug!("Old connection close during reconnect: {:#}", e);
        }
        tracing::info!("Reconnecting to RabbitMQ...");
        self.connect_with_confirms().await
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
