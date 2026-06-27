//! Live integration tests for the redis connection pool against a real
//! Valkey container. Connection lifecycle (creation, recycling, reuse) is
//! owned by `deadpool`; here we prove the redis wiring works end-to-end
//! against the real protocol: command and pipeline round-trips, concurrent
//! checkouts over a bounded pool, saturation→wait→timeout, and recovery
//! after a server-side connection kill. One test
//! (`create_fails_fast_on_dead_redis`) needs no container — it probes a
//! refused port to prove the eager construction probe fails fast.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use air_elt_commons_redis::{RedisPool, RedisPoolConfig, RedisPoolError};
use air_elt_commons_testing::valkey::valkey_handle;
use air_elt_monitoring::PoolStatsReader;

fn config(max_connections: u32) -> RedisPoolConfig {
    RedisPoolConfig {
        max_connections: Some(max_connections),
        acquire_timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_get_round_trip() {
    let handle = valkey_handle().await.unwrap();
    let pool = RedisPool::create(&handle.url, &config(2)).await.unwrap();

    // The configured pool size is what the sink reports as max_connections().
    assert_eq!(pool.max_connections(), 2);

    let key = handle.key("rt");
    let mut conn = pool.acquire().await.unwrap();
    let _: () = conn
        .query(redis::cmd("SET").arg(&key).arg("hello"))
        .await
        .unwrap();
    let got: String = conn.query(redis::cmd("GET").arg(&key)).await.unwrap();
    assert_eq!(got, "hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipeline_round_trip() {
    let handle = valkey_handle().await.unwrap();
    let pool = RedisPool::create(&handle.url, &config(2)).await.unwrap();

    let key = handle.key("pipe");
    let mut pipe = redis::Pipeline::with_capacity(2);
    pipe.add_command(redis::cmd("SET").arg(&key).arg("world").clone());
    pipe.add_command(redis::cmd("GET").arg(&key).clone());

    let mut conn = pool.acquire().await.unwrap();
    // The whole pipeline rides one connection in a single round-trip.
    let (_set, got): ((), String) = conn.query_pipeline(&pipe).await.unwrap();
    assert_eq!(got, "world");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_commands_over_bounded_pool() {
    let handle = valkey_handle().await.unwrap();
    // Only two connections; 40 concurrent tasks must share them (queueing
    // on the pool), and every task must still complete correctly. This test
    // asserts correctness under contention, NOT the acquire timeout, so the
    // wait is generous: under heavy parallel-suite load the default helper's
    // 300ms cap spuriously trips `AcquireTimeout` while 40 tasks drain
    // through 2 connections (each checkout also pays a recycle PING).
    let cfg = RedisPoolConfig {
        max_connections: Some(2),
        acquire_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    };
    let pool = RedisPool::create(&handle.url, &cfg).await.unwrap();

    let mut tasks = Vec::new();
    for i in 0..40u32 {
        let pool = pool.clone();
        let key = handle.key(&format!("mux:{i}"));
        tasks.push(tokio::spawn(async move {
            let mut conn = pool.acquire().await.unwrap();
            let _: () = conn
                .query(redis::cmd("SET").arg(&key).arg(i))
                .await
                .unwrap();
            let got: u32 = conn.query(redis::cmd("GET").arg(&key)).await.unwrap();
            assert_eq!(got, i);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    // All guards dropped → nothing checked out; the pool never opened more
    // than `max_connections` sockets. Read through the real monitoring path
    // (`stats_reader().read()`), the value an operator's gauge shows.
    let counts = pool.stats_reader().read();
    assert_eq!(counts.active, 0, "all checkouts returned");
    assert!(
        counts.idle <= 2,
        "live connections never exceeded the pool size"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saturation_waits_then_times_out() {
    let handle = valkey_handle().await.unwrap();
    // Single connection so one checkout saturates the pool.
    let pool = RedisPool::create(&handle.url, &config(1)).await.unwrap();

    let held = pool.acquire().await.unwrap();

    // While the single connection is held, the monitoring reader (the
    // operator-facing active/idle gauge) must report it busy: one active,
    // none idle. This also exercises the real `status()` → active/idle
    // split at its discriminating (active > 0) state.
    let counts = pool.stats_reader().read();
    assert_eq!(
        (counts.active, counts.idle),
        (1, 0),
        "a held connection in a size-1 pool reads as active"
    );

    // Second checkout: pool saturated → waits up to acquire-timeout, then
    // errors with the saturation-specific classification (not a generic
    // backend/unavailable error the runner would surface and retry
    // differently).
    let second_err = pool.acquire().await.err();
    assert!(
        matches!(second_err, Some(RedisPoolError::AcquireTimeout)),
        "saturated acquire must classify as AcquireTimeout, got {second_err:?}"
    );

    // Free the connection → a fresh checkout succeeds.
    drop(held);
    let recovered = pool.acquire().await;
    assert!(
        recovered.is_ok(),
        "a freed connection must let acquire succeed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_fails_fast_on_dead_redis() {
    // No container: a refused port must surface at construction (via the
    // eager get() + PING probe), not silently defer to the first batch.
    // Short timeouts keep the test fast.
    let cfg = RedisPoolConfig {
        max_connections: Some(1),
        connect_timeout: Some(Duration::from_millis(500)),
        acquire_timeout: Some(Duration::from_millis(500)),
        ..Default::default()
    };
    let result = RedisPool::create("redis://127.0.0.1:1", &cfg).await;
    assert!(
        result.is_err(),
        "create() must fail fast against a dead redis"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_connection_recovers() {
    let handle = valkey_handle().await.unwrap();
    let pool = RedisPool::create(&handle.url, &config(2)).await.unwrap();

    // Check out a connection and learn its server-side client id.
    let mut conn = pool.acquire().await.unwrap();
    let client_id: i64 = conn.query(redis::cmd("CLIENT").arg("ID")).await.unwrap();

    // Kill that connection from a separate admin connection.
    let admin_client = redis::Client::open(handle.url.as_str()).unwrap();
    let mut admin = admin_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let _: redis::Value = redis::cmd("CLIENT")
        .arg("KILL")
        .arg("ID")
        .arg(client_id)
        .query_async(&mut admin)
        .await
        .unwrap();

    // The next command on the killed connection errors.
    let dead: Result<String, _> = conn.query(&redis::cmd("PING")).await;
    assert!(dead.is_err(), "command on a killed connection must error");
    drop(conn);

    // deadpool recycles (health-checks) the broken connection on the next
    // checkout, discarding it and dialing a fresh one — so the pool keeps
    // serving without operator intervention.
    let mut next = pool.acquire().await.unwrap();
    let pong: String = next.query(&redis::cmd("PING")).await.unwrap();
    assert_eq!(pong, "PONG");
}
