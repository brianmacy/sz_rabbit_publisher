use anyhow::{Context, Result};
use futures_core::Stream;
use lapin::{
    BasicProperties, Channel, Confirmation, Connection, ConnectionProperties, Event,
    PublisherConfirm, message::BasicReturnMessage, options::*, types::ShortString,
};
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

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
    /// Skip the first N non-empty records before publishing (resume support).
    pub skip_lines: u64,
    /// AMQP `delivery-mode`: `true` = 2 (persistent, written to the broker's message
    /// store), `false` = 1 (transient, may stay in memory).
    ///
    /// Persistent is the default because losing queued records is usually worse than
    /// the I/O. On a throughput benchmark it is close to pure overhead: every message
    /// is written to disk on publish and read back to deliver. Measured on a 1B-record
    /// run (RabbitMQ 3.12.10, durable classic queue, ~1,500 msg/s): ~1,530 disk
    /// reads/s and ~1,530 writes/s with only 2% of a 49k-deep queue resident in RAM,
    /// plus broker flow control (3.6% of publishes nacked, 7.9M throttle events).
    ///
    /// Only classic queues honour this: quorum queues persist every message to disk
    /// regardless of delivery mode (<https://www.rabbitmq.com/docs/quorum-queues>).
    /// Publisher confirms, mandatory returns and retries are unchanged either way.
    ///
    /// Transient messages in a classic queue are LOST if the broker restarts. Do not
    /// use it when the queue is the only copy of the data, or while investigating
    /// record loss.
    pub persistent: bool,
}

/// Maps the config flag onto the AMQP `delivery-mode` byte: 2 = persistent (broker
/// writes the message to its store), 1 = transient. Extracted so the mapping is
/// unit-testable without a live broker — getting it backwards would silently make
/// every benchmark either durable-and-slow or transient-and-lossy.
fn delivery_mode(persistent: bool) -> u8 {
    if persistent { 2 } else { 1 }
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

    /// Live snapshot of this publisher's statistics (safe to call while
    /// `publish_file` is running on another task).
    pub fn stats(&self) -> crate::stats::Stats {
        self.stats.get_snapshot()
    }

    /// The `--skip-lines` this publisher was started with.
    pub fn skip_lines(&self) -> u64 {
        self.config.skip_lines
    }

    /// Publishes all lines from a file to RabbitMQ
    /// Returns statistics about the publishing operation
    pub async fn publish_file(&self, file_path: &str) -> Result<crate::stats::Stats> {
        tracing::info!("Opening file: {}", file_path);
        let mut reader = FileReader::open(file_path)
            .await
            .context("Failed to open input file")?;

        // Resume support: discard the first N already-confirmed records. No seek on
        // compressed input, so this decodes through the stream. Skipped records are
        // never sent: over-skipping loses data; under-skipping only re-sends.
        if self.config.skip_lines > 0 {
            tracing::warn!(
                "Resuming: skipping first {} records of {} — they are NOT published by this run; \
                 if any of them were never confirmed by a previous run, they are lost",
                self.config.skip_lines,
                file_path
            );
            let skipped = reader
                .skip(self.config.skip_lines)
                .context("Failed while skipping records for resume")?;
            if skipped < self.config.skip_lines {
                tracing::warn!(
                    "Reached EOF after skipping only {} of {} requested records — nothing to publish",
                    skipped,
                    self.config.skip_lines
                );
            } else {
                tracing::info!("Skip complete ({skipped} records); publishing from here");
            }
        }

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
        let (tx, rx) = mpsc::channel::<Message>(self.config.max_pending * 2);
        let reader_handle = spawn_reader(reader, tx, self.stats.clone());

        self.publish_all(rx, &mut connection, &mut channel).await;

        // The reader has finished (the publish loop only exits once the channel is
        // closed). Its result says whether the WHOLE file was read.
        let read_result = reader_handle.await.context("File reader task panicked")?;

        // Close connection gracefully
        if let Err(e) = connection.close(0, "Publishing complete".into()).await {
            tracing::warn!("Failed to close connection gracefully: {:#}", e);
        }

        self.stats.print_final_summary();
        let stats = self.stats.get_snapshot();

        // A read error ends the file early: everything read WAS confirmed, but the
        // rest of the file was never published. That must never look like success.
        read_result.with_context(|| {
            format!(
                "Input read failed after {n} records; those {n} were confirmed by the broker \
                 but the rest of the file was NOT published. Fix the input and resume with \
                 --skip-lines {n}",
                n = self.config.skip_lines + stats.confirmed_prefix
            )
        })?;
        ensure_all_acked(&stats)?;

        Ok(stats)
    }

    /// Publish every message from `rx` until the channel closes AND every
    /// message is confirmed. Never drops a message: nacks, unroutable returns
    /// and connection failures all put the message back for re-publishing.
    async fn publish_all(
        &self,
        mut rx: mpsc::Receiver<Message>,
        connection: &mut Connection,
        channel: &mut Channel,
    ) {
        let mut pending: Vec<(Message, PublisherConfirm)> =
            Vec::with_capacity(self.config.max_pending);
        let mut unsent: Vec<Message> = Vec::new();
        let mut eof = false;
        let mut received = 0u64;

        while !eof || !unsent.is_empty() || !pending.is_empty() {
            // Fill batch: unsent (nacked/returned/unconfirmed) messages first, then new
            let mut conn_failed = false;
            while pending.len() < self.config.max_pending {
                let message = if let Some(msg) = unsent.pop() {
                    msg
                } else if eof {
                    break;
                } else {
                    match rx.recv().await {
                        Some(msg) => {
                            received += 1;
                            msg
                        }
                        None => {
                            eof = true;
                            break;
                        }
                    }
                };
                match self.publish_one(channel, &message.body).await {
                    Ok(confirm) => {
                        self.stats.increment_pending();
                        pending.push((message, confirm));
                    }
                    Err(e) => {
                        tracing::warn!("Publish failed: {:#}, reconnecting...", e);
                        unsent.push(message);
                        conn_failed = true;
                        break;
                    }
                }
            }

            let outcome = if conn_failed {
                DrainOutcome::ConnectionFailed
            } else {
                self.drain_confirms(&mut pending, &mut unsent, connection)
                    .await
            };

            match outcome {
                DrainOutcome::AllAcked => {}
                DrainOutcome::Retry => sleep(self.config.retry_delay).await,
                DrainOutcome::ConnectionFailed => {
                    // Outcome of every still-pending publish is unknown: re-publish
                    // them all (at-least-once; reported as `republished`).
                    let n = pending.len() as u64;
                    unsent.extend(pending.drain(..).map(|(msg, _)| msg));
                    self.stats.requeue_unconfirmed(n);
                    (*connection, *channel) = self.reconnect(connection).await;
                }
            }
            self.stats.set_confirmed_prefix(confirmed_prefix(
                pending
                    .iter()
                    .map(|(m, _)| m.seq)
                    .chain(unsent.iter().map(|m| m.seq)),
                received,
            ));
        }
    }

    /// Publish one persistent, mandatory message. `mandatory` makes the broker
    /// RETURN a message it cannot route (missing exchange binding) instead of
    /// acking and silently discarding it.
    async fn publish_one(
        &self,
        channel: &Channel,
        message: &[u8],
    ) -> lapin::Result<PublisherConfirm> {
        channel
            .basic_publish(
                ShortString::from(self.config.exchange.as_str()),
                ShortString::from(self.config.routing_key.as_str()),
                BasicPublishOptions {
                    mandatory: true,
                    ..BasicPublishOptions::default()
                },
                message,
                BasicProperties::default()
                    .with_delivery_mode(delivery_mode(self.config.persistent)),
            )
            .await
    }

    /// Await every pending confirm. Acks are counted; nacks and unroutable
    /// returns go back to `unsent`. On a connection failure, the failed message
    /// goes back to `unsent` and the rest stay in `pending` for the caller.
    ///
    /// lapin (4.10) attaches a `basic.return` to an ARBITRARY tag of a
    /// coalesced `basic.ack multiple=true` (it iterates a HashMap), so in a
    /// batch with any return, a plain `Ack(None)` may belong to the message
    /// that was really returned. Such a batch's `Ack(None)` messages are
    /// therefore re-published (possible duplicates, counted `republished`)
    /// rather than counted acked.
    async fn drain_confirms(
        &self,
        pending: &mut Vec<(Message, PublisherConfirm)>,
        unsent: &mut Vec<Message>,
        connection: &Connection,
    ) -> DrainOutcome {
        let mut returned: Option<BasicReturnMessage> = None;
        let mut returned_count = 0u64;
        let mut nacked = 0u64;
        let mut acked: Vec<Message> = Vec::new();
        let mut iter = std::mem::take(pending).into_iter();

        while let Some((message, mut confirm)) = iter.next() {
            match self.await_confirm(&mut confirm, connection).await {
                Ok(Confirmation::Ack(None)) => acked.push(message),
                Ok(Confirmation::Ack(Some(ret)) | Confirmation::Nack(Some(ret))) => {
                    self.stats.increment_returned();
                    returned_count += 1;
                    returned = Some(ret);
                    unsent.push(message);
                }
                Ok(Confirmation::Nack(None) | Confirmation::NotRequested) => {
                    self.stats.increment_nacked();
                    nacked += 1;
                    unsent.push(message);
                }
                Err(e) => {
                    tracing::warn!("Confirm failed: {:#}, reconnecting...", e);
                    // The batch's returns are now unknowable: its acks are unproven too.
                    acked.push(message);
                    self.stats.requeue_unconfirmed(acked.len() as u64);
                    unsent.append(&mut acked);
                    pending.extend(iter);
                    return DrainOutcome::ConnectionFailed;
                }
            }
        }

        if returned_count > 0 && !acked.is_empty() {
            tracing::warn!(
                "Re-publishing {} acked message(s) from a batch with unroutable returns: \
                 the client cannot tell which ack hid a return (possible duplicates)",
                acked.len()
            );
            self.stats.requeue_unconfirmed(acked.len() as u64);
            unsent.append(&mut acked);
        }
        for _ in acked {
            self.stats.increment_acked();
        }

        if let Some(ret) = returned {
            tracing::error!(
                "{} message(s) UNROUTABLE: broker returned them ({} {}) for exchange '{}' \
                 routing key '{}' — no queue is bound. They are NOT counted as acked and \
                 will be retried every {:?} until a binding exists.",
                returned_count,
                ret.reply_code,
                ret.reply_text,
                self.config.exchange,
                self.config.routing_key,
                self.config.retry_delay
            );
        }
        if nacked > 0 {
            tracing::warn!(
                "{} publish(es) NACKED by the broker (e.g. queue full with x-overflow=reject-publish); \
                 retrying in {:?}",
                nacked,
                self.config.retry_delay
            );
        }
        if nacked > 0 || returned_count > 0 {
            DrainOutcome::Retry
        } else {
            DrainOutcome::AllAcked
        }
    }

    /// Await one confirm, logging loudly (but never giving up) while the broker
    /// withholds it — e.g. a memory/disk alarm has blocked the connection.
    async fn await_confirm(
        &self,
        confirm: &mut PublisherConfirm,
        connection: &Connection,
    ) -> lapin::Result<Confirmation> {
        loop {
            match timeout(STALL_REPORT_INTERVAL, &mut *confirm).await {
                Ok(result) => return result,
                Err(_) => tracing::warn!(
                    "No broker confirm for {:?} (connection blocked by broker resource alarm: {}); \
                     publishing is paused, still waiting — no records are dropped",
                    STALL_REPORT_INTERVAL,
                    connection.status().blocked()
                ),
            }
        }
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
        // Bounded: a broker that is mid-shutdown/startup can accept the TCP
        // connection and then never finish the handshake; without a bound the
        // publisher hangs forever instead of retrying.
        timeout(CONNECT_TIMEOUT, self.try_connect_unbounded())
            .await
            .context("Timed out connecting to RabbitMQ")?
    }

    async fn try_connect_unbounded(&self) -> Result<(Connection, Channel)> {
        let conn = Connection::connect(&self.config.amqp_url, ConnectionProperties::default())
            .await
            .context("Failed to connect to RabbitMQ")?;
        tokio::spawn(log_connection_events(Box::pin(conn.events_listener())));
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
        match timeout(CONNECT_TIMEOUT, old_conn.close(0, "reconnecting".into())).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::debug!("Old connection close during reconnect: {:#}", e),
            Err(_) => tracing::debug!("Old connection close timed out during reconnect"),
        }
        tracing::info!("Reconnecting to RabbitMQ...");
        self.connect_with_confirms().await
    }
}

/// A raw record (one input line, without its terminator) and its 0-based
/// position among the records this run read.
#[derive(Debug)]
struct Message {
    seq: u64,
    body: Vec<u8>,
}

/// Number of leading records (in file order) that are ALL confirmed: every
/// record below the lowest one still pending/unsent, or not yet received.
fn confirmed_prefix(outstanding: impl Iterator<Item = u64>, received: u64) -> u64 {
    outstanding.fold(received, u64::min)
}

/// Log broker flow-control events: a resource alarm (memory/disk) blocks
/// publishing, which otherwise looks exactly like a hang.
async fn log_connection_events(mut events: Pin<Box<dyn Stream<Item = Event> + Send>>) {
    while let Some(event) = std::future::poll_fn(|cx| events.as_mut().poll_next(cx)).await {
        match event {
            Event::ConnectionBlocked(reason) => tracing::warn!(
                "Broker BLOCKED this connection ({reason}): publishing is paused until the \
                 alarm clears; no records are dropped"
            ),
            Event::ConnectionUnblocked => {
                tracing::warn!("Broker UNBLOCKED this connection: publishing resumes")
            }
            _ => {}
        }
    }
}

/// Upper bound on one connect (or close) attempt before it is retried.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How often to warn while waiting on a confirm the broker is withholding.
const STALL_REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// Result of draining a batch of publisher confirms.
#[derive(Debug, PartialEq)]
enum DrainOutcome {
    /// Every message in the batch was acked (and routed).
    AllAcked,
    /// Some messages were nacked or returned unroutable; back off, then retry.
    Retry,
    /// The connection/channel failed; reconnect and re-publish what is pending.
    ConnectionFailed,
}

/// Read the file on a separate task, feeding records into the bounded channel
/// (which is what blocks the reader when publishing falls behind). Returns
/// `Err` if the file could not be fully read.
fn spawn_reader(
    mut reader: FileReader,
    tx: mpsc::Sender<Message>,
    stats: StatsTracker,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let mut seq = 0u64;
        while let Some(line) = reader.next_line() {
            let line = Message { seq, body: line? };
            seq += 1;
            stats.increment_total();
            // Try non-blocking send first to detect back pressure
            let sent = match tx.try_send(line) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(msg)) => {
                    stats.increment_throttled();
                    tx.send(msg).await.map_err(|_| ())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
            };
            if sent.is_err() {
                anyhow::bail!("Publisher stopped accepting records (channel closed)");
            }
        }
        tracing::info!("File reading complete: {} lines read", reader.lines_read());
        Ok(())
    })
}

/// Final invariant: a successful run confirmed exactly one ack per record read.
fn ensure_all_acked(stats: &crate::stats::Stats) -> Result<()> {
    if stats.acked != stats.total_records {
        anyhow::bail!(
            "Delivery invariant violated: read {} records but {} were acked",
            stats.total_records,
            stats.acked
        );
    }
    Ok(())
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
            skip_lines: 0,
            persistent: true,
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
            skip_lines: 0,
            persistent: true,
        };

        let publisher = RabbitMQPublisher::new(config);
        assert_eq!(publisher.config.amqp_url, "amqp://localhost");
    }

    #[test]
    fn test_confirmed_prefix() {
        // Nothing outstanding: everything received is confirmed.
        assert_eq!(confirmed_prefix(std::iter::empty(), 10), 10);
        // Record 3 still unconfirmed: only 0..3 are a safe resume prefix.
        assert_eq!(confirmed_prefix([7, 3, 9].into_iter(), 10), 3);
    }

    #[test]
    fn test_ensure_all_acked() {
        let mut stats = crate::stats::Stats::new();
        stats.total_records = 5;
        stats.acked = 5;
        assert!(ensure_all_acked(&stats).is_ok());
        stats.acked = 4;
        assert!(ensure_all_acked(&stats).is_err());
    }

    #[test]
    fn delivery_mode_maps_persistent_to_2_and_transient_to_1() {
        // AMQP 0-9-1 basic.properties delivery-mode: 1 = non-persistent, 2 = persistent.
        assert_eq!(delivery_mode(true), 2, "persistent must be delivery-mode 2");
        assert_eq!(delivery_mode(false), 1, "transient must be delivery-mode 1");
    }
}
