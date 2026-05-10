//! Concurrent `save_cursor` against CockroachDB to exercise the
//! `with_serialization_retry` wrapper.
//!
//! N tasks race on the same flow row. CockroachDB's SERIALIZABLE isolation
//! turns conflicting transactions into `40001 RETRY_SERIALIZABLE`, which the
//! storage wraps via `air_elt_commons_pg::retry::with_serialization_retry`.
//! All tasks must succeed, and the final `load_cursor` must return one of
//! the written states.
//!
//! We use 8 concurrent writers and a small per-task burst so the test
//! reliably traverses the retry path under low test-machine load. With only
//! two writers Cockroach often serialises trivially and the retry branch
//! never fires, which would let a regression in the wrapper slip through.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Storage;
use air_elt_core::types::Value;
use air_elt_storage_postgres::{PgStorage, PgStorageConfig};

const WRITERS: usize = 8;
const BURST_PER_WRITER: usize = 4;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cockroach_save_cursor_handles_serialization_retry() {
    let handle = cockroach_pool().await;
    let storage = Arc::new(
        PgStorage::connect(PgStorageConfig {
            dialect: Dialect::Cockroach,
            url: handle.url_with_database(),
            ..Default::default()
        })
        .await
        .expect("connect cockroach storage"),
    );
    storage.migrate().await.expect("migrate");

    let flow = "flow_concurrent";

    let mut handles = Vec::with_capacity(WRITERS);
    for w in 0..WRITERS {
        let storage = Arc::clone(&storage);
        handles.push(tokio::spawn(async move {
            for i in 0..BURST_PER_WRITER {
                let state = CursorState::new(vec![CursorFieldValue {
                    name: "id".into(),
                    value: Value::Int64((w * 1000 + i) as i64),
                }]);
                storage.save_cursor(flow, &state, false).await?;
            }
            Ok::<_, air_elt_core::error::RuntimeError>(())
        }));
    }

    for h in handles {
        h.await.expect("task join").expect("save_cursor succeeded");
    }

    let loaded = storage
        .load_cursor(flow)
        .await
        .expect("load_cursor")
        .expect("cursor present after concurrent saves");
    let id = match &loaded.fields[0].value {
        Value::Int64(n) => *n,
        other => panic!("unexpected cursor value: {other:?}"),
    };
    // The winning writer's last `i` must be in [0, BURST_PER_WRITER) and
    // `w` in [0, WRITERS); reverse-derive both and assert ranges.
    let w = id / 1000;
    let i = id % 1000;
    assert!(
        (0..WRITERS as i64).contains(&w),
        "writer index out of range: {w}"
    );
    assert!(
        (0..BURST_PER_WRITER as i64).contains(&i),
        "burst index out of range: {i}"
    );

    handle.pool.close().await;
}
