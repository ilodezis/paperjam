//! The background committer: when it commits, when it lets go of the writer,
//! and that it stays off the blocking pool while there is nothing to do.
//! Time is paused, so minutes of idling take no real time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use tokio_util::sync::CancellationToken;

use super::{
    COMMIT_INTERVAL, IDLE_TICKS_BEFORE_RELEASE, IndexDoc, SearchIndex, Step, committer_step,
};
use crate::ids::DocumentId;

fn doc(title: &str) -> IndexDoc {
    IndexDoc {
        id: DocumentId::new(),
        title: title.to_string(),
        texts: vec![title.to_string()],
        other_texts: Vec::new(),
        summary: None,
        type_key: None,
        correspondent_ids: Vec::new(),
        correspondent: Vec::new(),
        organization_names: Vec::new(),
        people: Vec::new(),
        tags: Vec::new(),
        fields: BTreeMap::new(),
        field_order: Vec::new(),
        summary_fields: Vec::new(),
        type_names: Vec::new(),
        document_date: None,
        added_at: Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
        local_only: false,
        attention: Vec::new(),
        trashed: false,
        has_pages: true,
    }
}

/// Idle time that is certainly past the release threshold.
fn long_idle() -> Duration {
    COMMIT_INTERVAL * (IDLE_TICKS_BEFORE_RELEASE + 5)
}

/// Waits (in paused time) until the committer has let go of the writer.
async fn wait_for_release(index: &SearchIndex) -> bool {
    for _ in 0..(IDLE_TICKS_BEFORE_RELEASE * 2) {
        if !index.has_writer() {
            return true;
        }
        tokio::time::sleep(COMMIT_INTERVAL).await;
    }
    !index.has_writer()
}

#[tokio::test(start_paused = true)]
async fn writer_is_released_after_an_edit_that_a_search_committed() {
    let index = SearchIndex::open_in_ram();
    let cancel = CancellationToken::new();
    let committer = index.spawn_committer(cancel.clone());

    // The server has been idle for a long time: the counter is far past the
    // threshold when the edit arrives (this is what hid the bug).
    tokio::time::sleep(long_idle()).await;
    assert!(!index.has_writer());

    // An edit followed at once by a search, which commits on its own before
    // the committer's next tick ever sees the pending change.
    index.index_document(&doc("invoice")).unwrap();
    index.commit_if_dirty().unwrap();
    assert!(index.has_writer());

    // Not released while it is fresh...
    tokio::time::sleep(COMMIT_INTERVAL * (IDLE_TICKS_BEFORE_RELEASE - 5)).await;
    assert!(index.has_writer(), "released too early");
    // ...and gone after the idle threshold.
    assert!(wait_for_release(&index).await, "writer was never released");

    // The next edit gets a new writer and the data survived the release.
    index.index_document(&doc("receipt")).unwrap();
    index.commit().unwrap();
    assert_eq!(index.num_docs(), 2);

    cancel.cancel();
    committer.await.unwrap();
    assert!(!index.has_writer());
}

#[tokio::test(start_paused = true)]
async fn writer_is_released_after_a_direct_rebuild_style_commit() {
    let index = SearchIndex::open_in_ram();
    let cancel = CancellationToken::new();
    let committer = index.spawn_committer(cancel.clone());
    tokio::time::sleep(long_idle()).await;

    index.clear().unwrap();
    index.index_document(&doc("rebuilt")).unwrap();
    index.commit().unwrap();

    assert!(wait_for_release(&index).await, "writer was never released");
    assert_eq!(index.num_docs(), 1);
    cancel.cancel();
    committer.await.unwrap();
}

/// Counts the threads the runtime spawns. A current-thread runtime has no
/// workers of its own, so every thread it starts is a blocking-pool thread.
#[test]
fn an_idle_committer_never_starts_a_blocking_thread() {
    let spawned = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&spawned);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .on_thread_start(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();
    runtime.block_on(async {
        let index = SearchIndex::open_in_ram();
        let cancel = CancellationToken::new();
        let committer = index.spawn_committer(cancel.clone());

        tokio::time::sleep(long_idle() * 3).await;
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            0,
            "nothing to commit or release, yet a blocking thread was started"
        );

        // Real work does use one, and the index still ends up committed.
        index.index_document(&doc("invoice")).unwrap();
        tokio::time::sleep(COMMIT_INTERVAL * 2).await;
        assert_eq!(index.num_docs(), 1, "the committer committed the edit");
        assert!(spawned.load(Ordering::SeqCst) > 0);

        cancel.cancel();
        committer.await.unwrap();
    });
}

#[test]
fn step_does_nothing_without_a_writer_or_a_change() {
    let mut idle = 0;
    for _ in 0..(IDLE_TICKS_BEFORE_RELEASE * 3) {
        assert_eq!(committer_step(&mut idle, false, false, false), Step::Idle);
    }
}

#[test]
fn step_commits_a_pending_change_and_restarts_the_idle_count() {
    let mut idle = IDLE_TICKS_BEFORE_RELEASE + 10;
    assert_eq!(
        committer_step(&mut idle, true, true, true),
        Step::Run {
            commit: true,
            release: false
        }
    );
    assert_eq!(idle, 0);
}

#[test]
fn step_releases_only_a_writer_that_exists_and_has_been_idle_long_enough() {
    let mut idle = 0;
    for tick in 1..IDLE_TICKS_BEFORE_RELEASE {
        assert_eq!(
            committer_step(&mut idle, false, false, true),
            Step::Idle,
            "tick {tick}"
        );
    }
    assert_eq!(
        committer_step(&mut idle, false, false, true),
        Step::Run {
            commit: false,
            release: true
        }
    );
    // A use that the committer did not have to commit (a search committed
    // it) still counts as use.
    assert_eq!(committer_step(&mut idle, false, true, true), Step::Idle);
    assert_eq!(idle, 0);
}
