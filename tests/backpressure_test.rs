//! "Never drop a record" back-pressure tests against a REAL RabbitMQ.
//!
//! Every test starts its own throwaway broker container (so it can trigger
//! resource alarms and restarts without disturbing anything else), publishes N
//! records from a file through the real publisher, then independently drains
//! the queue and asserts EVERY record id arrived at least once. Duplicates
//! (allowed: delivery is at-least-once) are reported.
//!
//! These need Docker, so they are `#[ignore]`d in a plain `cargo test`. Run them
//! with:
//!
//! ```text
//! cargo test --test backpressure_test -- --ignored
//! ```
//!
//! When run, missing infrastructure is a hard FAILURE, never a skip.
//! `TEST_RABBITMQ_IMAGE` overrides the broker image (default `rabbitmq:3-management`).
//! `TEST_QUEUE_TYPE` selects the queue type (default `quorum`, the production
//! configuration; `classic` also supported).

use anyhow::{Context, Result, bail, ensure};
use lapin::{
    Channel, Connection, ConnectionProperties,
    options::*,
    types::{AMQPValue, FieldTable},
};
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use sz_rabbit_publisher::{PublisherConfig, RabbitMQPublisher, Stats};
use tempfile::NamedTempFile;

const EXCHANGE: &str = "bp-exchange";
const QUEUE: &str = "bp-queue";
const ROUTING_KEY: &str = "bp.records";

/// A dedicated RabbitMQ container, removed on drop.
struct Broker {
    name: String,
    amqp_url: String,
}

impl Broker {
    fn start(test: &str) -> Result<Self> {
        // Publisher logs (warnings by default; RUST_LOG overrides) go to the test output.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();
        let image = std::env::var("TEST_RABBITMQ_IMAGE")
            .unwrap_or_else(|_| "rabbitmq:3-management".to_string());
        let name = format!("szpub-bp-{test}-{}", std::process::id());
        // Remove a leftover from an aborted earlier run with the same pid.
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        docker(&[
            "run",
            "-d",
            "--name",
            &name,
            "-p",
            "127.0.0.1::5672",
            &image,
        ])
        .context("docker is required for the back-pressure tests")?;
        let port = docker(&["port", &name, "5672/tcp"])?;
        let port = port
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .context("no mapped AMQP port")?
            .trim()
            .to_string();
        Ok(Self {
            amqp_url: format!("amqp://guest:guest@127.0.0.1:{port}/%2F"),
            name,
        })
    }

    async fn wait_ready(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let last_err = match self.ctl(&["await_startup"]).await {
                Err(e) => format!("{e:#}"),
                Ok(_) => {
                    match Connection::connect(&self.amqp_url, ConnectionProperties::default()).await
                    {
                        Ok(conn) => {
                            conn.close(0, "probe".into()).await.ok();
                            return Ok(());
                        }
                        Err(e) => format!("{e:#}"),
                    }
                }
            };
            ensure!(
                Instant::now() < deadline,
                "broker {} not ready in 120s: {last_err}",
                self.name
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Run `rabbitmqctl` inside the container.
    async fn ctl(&self, args: &[&str]) -> Result<String> {
        // `-u rabbitmq`: an exec as root during boot would create the Erlang
        // cookie root-owned and crash the broker (eacces on .erlang.cookie).
        let mut full = vec!["exec", "-u", "rabbitmq", self.name.as_str(), "rabbitmqctl"];
        full.extend_from_slice(args);
        let out = tokio::process::Command::new("docker")
            .args(&full)
            .output()
            .await?;
        ensure!(
            out.status.success(),
            "rabbitmqctl {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn start_ready(test: &str) -> Result<Self> {
        let broker = Self::start(test)?;
        broker.wait_ready().await?;
        Ok(broker)
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}

fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .context("failed to run docker")?;
    ensure!(
        out.status.success(),
        "docker {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn open_channel(url: &str) -> Result<(Connection, Channel)> {
    let conn = Connection::connect(url, ConnectionProperties::default()).await?;
    let ch = conn.create_channel().await?;
    Ok((conn, ch))
}

/// Declare a durable exchange + durable queue (with `queue_args`), optionally bound.
async fn declare_topology(url: &str, mut queue_args: FieldTable, bind: bool) -> Result<()> {
    let queue_type = std::env::var("TEST_QUEUE_TYPE").unwrap_or_else(|_| "quorum".to_string());
    queue_args.insert(
        "x-queue-type".into(),
        AMQPValue::LongString(queue_type.into()),
    );
    let (conn, ch) = open_channel(url).await?;
    ch.exchange_declare(
        EXCHANGE.into(),
        lapin::ExchangeKind::Direct,
        ExchangeDeclareOptions {
            durable: true,
            ..Default::default()
        },
        FieldTable::default(),
    )
    .await?;
    ch.queue_declare(
        QUEUE.into(),
        QueueDeclareOptions {
            durable: true,
            ..Default::default()
        },
        queue_args,
    )
    .await?;
    if bind {
        bind_queue(&ch).await?;
    }
    conn.close(0, "setup".into()).await.ok();
    Ok(())
}

async fn bind_queue(ch: &Channel) -> Result<()> {
    ch.queue_bind(
        QUEUE.into(),
        EXCHANGE.into(),
        ROUTING_KEY.into(),
        QueueBindOptions::default(),
        FieldTable::default(),
    )
    .await?;
    Ok(())
}

fn record(i: u64) -> String {
    format!(r#"{{"DATA_SOURCE":"BP","RECORD_ID":"{i}","NAME_FULL":"Person {i}"}}"#)
}

fn write_records(n: u64) -> Result<NamedTempFile> {
    let mut f = NamedTempFile::new()?;
    for i in 0..n {
        writeln!(f, "{}", record(i))?;
    }
    f.flush()?;
    Ok(f)
}

fn record_id(body: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(body).ok()?;
    let rest = s.split(r#""RECORD_ID":""#).nth(1)?;
    rest.split('"').next()?.parse().ok()
}

fn config(url: &str, max_pending: usize, retry_delay: Duration) -> PublisherConfig {
    PublisherConfig {
        amqp_url: url.to_string(),
        exchange: EXCHANGE.to_string(),
        queue: QUEUE.to_string(),
        routing_key: ROUTING_KEY.to_string(),
        max_pending,
        retry_delay,
        report_interval: 1_000_000,
        skip_lines: 0,
    }
}

/// Run `publish_file` on its own task so faults can be injected meanwhile.
fn spawn_publish(
    publisher: Arc<RabbitMQPublisher>,
    file: &NamedTempFile,
) -> tokio::task::JoinHandle<Result<Stats>> {
    let path = file.path().to_str().unwrap().to_string();
    tokio::spawn(async move { publisher.publish_file(&path).await })
}

/// Wait until the publisher has acked at least `n`.
async fn wait_acked(publisher: &RabbitMQPublisher, n: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while publisher.stats().acked < n {
        ensure!(
            Instant::now() < deadline,
            "publisher never reached {n} acks (stats: {:?})",
            publisher.stats()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

async fn join_publish(handle: tokio::task::JoinHandle<Result<Stats>>) -> Result<Stats> {
    tokio::time::timeout(Duration::from_secs(180), handle)
        .await
        .context("publisher did not finish within 180s")??
}

/// Drain everything currently in the queue; returns record-id -> times seen.
async fn drain_queue(url: &str) -> Result<HashMap<u64, u64>> {
    let (conn, ch) = open_channel(url).await?;
    let mut seen = HashMap::new();
    while let Some(msg) = ch
        .basic_get(QUEUE.into(), BasicGetOptions { no_ack: true })
        .await?
    {
        let id = record_id(&msg.delivery.data).context("message without RECORD_ID")?;
        *seen.entry(id).or_insert(0) += 1;
    }
    conn.close(0, "drained".into()).await.ok();
    Ok(seen)
}

/// Every id in `0..n` must have been seen at least once. Returns duplicates.
fn assert_all_arrived(seen: &HashMap<u64, u64>, n: u64, test: &str) -> Result<u64> {
    let missing: Vec<u64> = (0..n).filter(|i| !seen.contains_key(i)).collect();
    if !missing.is_empty() {
        bail!(
            "{test}: {} of {n} records LOST (first: {:?})",
            missing.len(),
            &missing[..missing.len().min(10)]
        );
    }
    let extra: Vec<&u64> = seen.keys().filter(|id| **id >= n).collect();
    ensure!(extra.is_empty(), "{test}: unexpected ids {extra:?}");
    let delivered: u64 = seen.values().sum();
    let duplicates = delivered - n;
    eprintln!("{test}: all {n} records arrived; {delivered} delivered, {duplicates} duplicates");
    Ok(duplicates)
}

fn assert_stats(stats: &Stats, n: u64, test: &str) -> Result<()> {
    eprintln!("{test}: publisher stats {stats:?}");
    ensure!(
        stats.total_records == n,
        "{test}: read {}",
        stats.total_records
    );
    ensure!(stats.acked == n, "{test}: acked {} != {n}", stats.acked);
    Ok(())
}

/// 1. `x-max-length` + `reject-publish` while a slow consumer drains: the
///    broker nacks overflow, the publisher must retry until all N land.
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_overflow_reject_publish_retries_until_all_land() -> Result<()> {
    const N: u64 = 1000;
    let broker = Broker::start_ready("overflow").await?;
    let url = broker.amqp_url.clone();
    let mut args = FieldTable::default();
    args.insert("x-max-length".into(), AMQPValue::LongInt(50));
    args.insert(
        "x-overflow".into(),
        AMQPValue::LongString("reject-publish".into()),
    );
    declare_topology(&url, args, true).await?;

    // Slow consumer: ~2ms per message, so the 50-message queue stays full.
    let done = Arc::new(AtomicBool::new(false));
    let consumer = {
        let (url, done) = (url.clone(), done.clone());
        tokio::spawn(async move {
            let (conn, ch) = open_channel(&url).await?;
            let mut seen: HashMap<u64, u64> = HashMap::new();
            loop {
                match ch
                    .basic_get(QUEUE.into(), BasicGetOptions { no_ack: false })
                    .await?
                {
                    Some(msg) => {
                        let id = record_id(&msg.delivery.data).context("no RECORD_ID")?;
                        *seen.entry(id).or_insert(0) += 1;
                        msg.delivery.acker.ack(BasicAckOptions::default()).await?;
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    None if done.load(Ordering::SeqCst) => break,
                    None => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
            conn.close(0, "consumer done".into()).await.ok();
            anyhow::Ok(seen)
        })
    };

    let file = write_records(N)?;
    let publisher = Arc::new(RabbitMQPublisher::new(config(
        &url,
        200,
        Duration::from_millis(100),
    )));
    let stats = join_publish(spawn_publish(publisher, &file)).await;
    done.store(true, Ordering::SeqCst);
    let stats = stats?;
    let seen = consumer.await??;

    assert_stats(&stats, N, "overflow")?;
    ensure!(
        stats.nacked > 0,
        "overflow: broker never nacked — back pressure was not exercised"
    );
    assert_all_arrived(&seen, N, "overflow")?;
    Ok(())
}

/// 2. A memory alarm mid-publish blocks the connection: publishing must pause
///    (no acks, no errors, nothing dropped) and resume when the alarm clears.
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_memory_alarm_pauses_then_resumes() -> Result<()> {
    const N: u64 = 20_000;
    let broker = Broker::start_ready("alarm").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), true).await?;

    let file = write_records(N)?;
    let publisher = Arc::new(RabbitMQPublisher::new(config(
        &url,
        50,
        Duration::from_millis(200),
    )));
    let handle = spawn_publish(publisher.clone(), &file);

    wait_acked(&publisher, 2000).await?;
    broker
        .ctl(&["set_vm_memory_high_watermark", "0.0000001"])
        .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let acked_blocked = publisher.stats().acked;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let acked_after_hold = publisher.stats().acked;
    let finished_during_alarm = handle.is_finished();
    let alarms = broker.ctl(&["eval", "rabbit_alarm:get_alarms()."]).await?;
    broker.ctl(&["set_vm_memory_high_watermark", "0.4"]).await?;

    eprintln!(
        "alarm: acked {acked_blocked} -> {acked_after_hold} during 10s alarm; alarms={}",
        alarms.trim()
    );
    ensure!(
        alarms.contains("memory"),
        "alarm: memory alarm was not raised: {alarms}"
    );
    ensure!(
        !finished_during_alarm,
        "alarm: publisher finished while blocked"
    );
    ensure!(
        acked_after_hold == acked_blocked && acked_blocked < N,
        "alarm: publishing was not paused ({acked_blocked} -> {acked_after_hold})"
    );

    let stats = join_publish(handle).await?;
    assert_stats(&stats, N, "alarm")?;
    assert_all_arrived(&drain_queue(&url).await?, N, "alarm")?;
    Ok(())
}

/// 3. Connections killed and the broker app restarted mid-publish: reconnect,
///    re-publish unconfirmed messages; durable queue keeps everything.
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_connection_kill_and_broker_restart_lose_nothing() -> Result<()> {
    const N: u64 = 20_000;
    let broker = Broker::start_ready("restart").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), true).await?;

    let file = write_records(N)?;
    let publisher = Arc::new(RabbitMQPublisher::new(config(
        &url,
        20,
        Duration::from_millis(500),
    )));
    let handle = spawn_publish(publisher.clone(), &file);

    wait_acked(&publisher, 2000).await?;
    broker
        .ctl(&["close_all_connections", "back-pressure test kill"])
        .await?;
    let acked_at_kill = publisher.stats().acked;

    wait_acked(&publisher, acked_at_kill + 3000).await?;
    broker.ctl(&["stop_app"]).await?;
    let acked_at_stop = publisher.stats().acked;
    tokio::time::sleep(Duration::from_secs(3)).await;
    broker.ctl(&["start_app"]).await?;
    eprintln!("restart: killed connections at acked={acked_at_kill}, stop_app at {acked_at_stop}");
    ensure!(
        acked_at_stop < N,
        "restart: publish finished before the restart"
    );

    let stats = join_publish(handle).await?;
    assert_stats(&stats, N, "restart")?;
    let dups = assert_all_arrived(&drain_queue(&url).await?, N, "restart")?;
    ensure!(
        dups <= stats.republished,
        "restart: {dups} duplicates but only {} republished were reported",
        stats.republished
    );
    Ok(())
}

/// 4. Unroutable (exchange exists, no binding): records must NOT be counted as
///    acked; once the binding is added they must all arrive.
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_unroutable_is_retried_not_acked() -> Result<()> {
    const N: u64 = 200;
    let broker = Broker::start_ready("unroutable").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), false).await?;

    let file = write_records(N)?;
    let publisher = Arc::new(RabbitMQPublisher::new(config(
        &url,
        100,
        Duration::from_millis(200),
    )));
    let handle = spawn_publish(publisher.clone(), &file);

    tokio::time::sleep(Duration::from_secs(4)).await;
    if handle.is_finished() {
        bail!(
            "unroutable: publisher finished while NO queue was bound — records were dropped \
             (result: {:?})",
            join_publish(handle).await
        );
    }
    let during = publisher.stats();
    eprintln!("unroutable: stats while unbound {during:?}");
    ensure!(
        during.acked == 0,
        "unroutable: {} counted acked",
        during.acked
    );
    ensure!(during.returned > 0, "unroutable: no returns recorded");

    let (conn, ch) = open_channel(&url).await?;
    bind_queue(&ch).await?;
    conn.close(0, "bound".into()).await.ok();

    let stats = join_publish(handle).await?;
    assert_stats(&stats, N, "unroutable")?;
    assert_all_arrived(&drain_queue(&url).await?, N, "unroutable")?;
    Ok(())
}

/// 5. A truncated (corrupt) gzip input must fail the run, not end it quietly
///    with exit 0 after publishing only the readable prefix.
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_truncated_input_fails_loudly() -> Result<()> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let broker = Broker::start_ready("truncated").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), true).await?;

    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    for i in 0..5000 {
        writeln!(enc, "{}", record(i))?;
    }
    let gz = enc.finish()?;
    let mut file = NamedTempFile::new()?;
    file.write_all(&gz[..gz.len() / 2])?;
    file.flush()?;

    let publisher = RabbitMQPublisher::new(config(&url, 100, Duration::from_millis(200)));
    let result = publisher.publish_file(file.path().to_str().unwrap()).await;
    eprintln!("truncated: result {result:?}");
    let err = match result {
        Ok(stats) => bail!(
            "truncated: corrupt input reported SUCCESS after {} of 5000 records",
            stats.total_records
        ),
        Err(e) => format!("{e:#}"),
    };
    ensure!(
        err.contains("--skip-lines"),
        "truncated: error must say how to resume: {err}"
    );
    // Everything that WAS read must have been delivered.
    let seen = drain_queue(&url).await?;
    let read = seen.len() as u64;
    ensure!(read > 0, "truncated: nothing delivered");
    assert_all_arrived(&seen, read, "truncated")?;
    Ok(())
}

/// 6. A line that is not valid UTF-8 must be published verbatim and must not
///    end the read (which would silently drop every record after it).
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_invalid_utf8_line_does_not_drop_the_rest() -> Result<()> {
    const N: u64 = 200;
    let broker = Broker::start_ready("utf8").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), true).await?;

    let mut file = NamedTempFile::new()?;
    for i in 0..N {
        if i == N / 2 {
            file.write_all(b"{\"RECORD_ID\":\"bad\xff\"}\n")?;
        }
        writeln!(file, "{}", record(i))?;
    }
    file.flush()?;

    let publisher = RabbitMQPublisher::new(config(&url, 100, Duration::from_millis(200)));
    let stats = publisher
        .publish_file(file.path().to_str().unwrap())
        .await?;
    assert_stats(&stats, N + 1, "utf8")?;

    let (conn, ch) = open_channel(&url).await?;
    let mut seen = HashMap::new();
    let mut bad = 0;
    while let Some(msg) = ch
        .basic_get(QUEUE.into(), BasicGetOptions { no_ack: true })
        .await?
    {
        match record_id(&msg.delivery.data) {
            Some(id) => *seen.entry(id).or_insert(0u64) += 1,
            None if msg.delivery.data == b"{\"RECORD_ID\":\"bad\xff\"}" => bad += 1,
            None => bail!("utf8: unexpected body {:?}", msg.delivery.data),
        }
    }
    conn.close(0, "drained".into()).await.ok();
    ensure!(bad == 1, "utf8: invalid-UTF-8 record delivered {bad} times");
    assert_all_arrived(&seen, N, "utf8")?;
    Ok(())
}

/// 7. A multi-member gzip (`cat a.gz b.gz`, pigz, bgzip) must publish EVERY
///    member, not stop silently after the first.
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_multi_member_gzip_publishes_every_member() -> Result<()> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    const N: u64 = 1000;

    let broker = Broker::start_ready("multigz").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), true).await?;

    let mut file = NamedTempFile::new()?;
    for half in [0..N / 2, N / 2..N] {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        for i in half {
            writeln!(enc, "{}", record(i))?;
        }
        file.write_all(&enc.finish()?)?;
    }
    file.flush()?;

    let publisher = RabbitMQPublisher::new(config(&url, 100, Duration::from_millis(200)));
    let stats = publisher
        .publish_file(file.path().to_str().unwrap())
        .await?;
    assert_stats(&stats, N, "multigz")?;
    assert_all_arrived(&drain_queue(&url).await?, N, "multigz")?;
    Ok(())
}

/// 8. SIGTERM while the broker has publishing blocked (memory alarm): the real
///    binary must log the block, exit non-zero (143) with a summary and an
///    exact resume point; every record before that point must be in the
///    queue; resuming from it must deliver the rest.
#[cfg(unix)]
#[tokio::test]
#[ignore = "needs Docker; run with --ignored"]
async fn test_sigterm_reports_exact_resume_point() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    const N: u64 = 20_000;

    let broker = Broker::start_ready("sigterm").await?;
    let url = broker.amqp_url.clone();
    declare_topology(&url, FieldTable::default(), true).await?;
    let file = write_records(N)?;
    let path = file.path().to_str().unwrap().to_string();
    let bin = env!("CARGO_BIN_EXE_sz_rabbit_publisher");
    let base_args = |extra: &[&str]| {
        let mut v: Vec<String> = [
            "-u",
            &url,
            "-e",
            EXCHANGE,
            "-q",
            QUEUE,
            "-r",
            ROUTING_KEY,
            "-m",
            "50",
            "--report-interval",
            "1000",
            "--retry-delay",
            "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        v.extend(extra.iter().map(|s| s.to_string()));
        v.push(path.clone());
        v
    };

    let mut child = tokio::process::Command::new(bin)
        .args(base_args(&[]))
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut stderr = child.stderr.take().unwrap();
    let mut output = String::new();

    // Wait for real progress (>= 2000 acked), then raise the memory alarm.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !output.contains("acked=2000") {
        ensure!(Instant::now() < deadline, "sigterm: no progress: {output}");
        let line = lines.next_line().await?.context("publisher exited early")?;
        output.push_str(&line);
        output.push('\n');
    }
    broker
        .ctl(&["set_vm_memory_high_watermark", "0.0000001"])
        .await?;
    // Collect output for a few seconds while blocked, then SIGTERM.
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(line)) = lines.next_line().await {
            output.push_str(&line);
            output.push('\n');
        }
    })
    .await;
    send_sigterm(child.id().context("no pid")?)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    while let Some(line) = lines.next_line().await? {
        output.push_str(&line);
        output.push('\n');
    }
    let mut err_text = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut err_text).await?;
    output.push_str(&err_text);
    broker.ctl(&["set_vm_memory_high_watermark", "0.4"]).await?;
    eprintln!("sigterm: exit {status:?}\n{output}");

    ensure!(
        status.code() == Some(143),
        "sigterm: exit status {status:?}"
    );
    ensure!(
        output.contains("Broker BLOCKED"),
        "sigterm: connection.blocked was not logged"
    );
    ensure!(
        output.contains("INTERRUPTED"),
        "sigterm: no interrupted report"
    );
    let resume: u64 = output
        .split("Resume this file with --skip-lines ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .context("sigterm: no resume point printed")?;
    ensure!(
        (2000..N).contains(&resume),
        "sigterm: implausible resume point {resume}"
    );

    // Everything before the resume point must already be in the queue.
    let mut seen = drain_queue(&url).await?;
    let before: HashMap<u64, u64> = seen
        .iter()
        .filter(|(id, _)| **id < resume)
        .map(|(k, v)| (*k, *v))
        .collect();
    assert_all_arrived(&before, resume, "sigterm-prefix")?;

    // Resume: the rest must arrive.
    let out = tokio::process::Command::new(bin)
        .args(base_args(&["--skip-lines", &resume.to_string()]))
        .output()
        .await?;
    ensure!(
        out.status.success(),
        "sigterm: resume run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (id, count) in drain_queue(&url).await? {
        *seen.entry(id).or_insert(0) += count;
    }
    assert_all_arrived(&seen, N, "sigterm-resumed")?;
    Ok(())
}

/// Send SIGTERM to `pid` via the `kill` utility (no libc dependency).
#[cfg(unix)]
fn send_sigterm(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    ensure!(status.success(), "kill -TERM {pid} failed");
    Ok(())
}
