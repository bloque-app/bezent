// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The watch on a submitted transaction, and the watchdog that ends it when it goes silent
//! (issue #84).
//!
//! A transaction is submitted with a status subscription, and the light client panics when one
//! transaction has two watchers and one closes (aledgent F-027 NOTES, decision 40), so the same
//! bytes are never watched twice at once: while a watch is alive, [`Watches::is_watching`] is true
//! and `resend` does not submit that hash again. A subscription is long-lived by nature (a
//! transaction can wait many blocks for its place), so it takes no request timeout. But a
//! subscription that has gone quiet for good, a peer gone without a close, would keep the hash
//! skipped until the process restarts.
//!
//! **The watchdog** ends a watch that has been silent for [`Watches::silence`]: the subscription is
//! dropped (and with it the light client's watcher) and the hash is free to be submitted again, the
//! same bytes, by the next `resend`. Silence is not an outcome. It does not say the transaction was
//! dropped, failed or never sent; nothing is recorded, nothing is refunded, and the transaction's
//! fate is found where it always is, in the chain's data by the proofs (`INV-8`, `REQ-RF-1`,
//! `REQ-NX-*`). It only says this endpoint stopped waiting for news, and will look again.

use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::task::JoinHandle;

/// The longest a watch may be silent, by default, before it is dropped: two minutes, about twenty
/// blocks. A healthy transaction is told of its progress (validated, broadcast, in a block,
/// finalized) within a few blocks of each step, and the era of a transaction is [`MAX_ERA`] blocks
/// (about six and a half minutes), so a longer bound would outlive most of the transaction's
/// life. A bound that is too short costs one more submission of the same bytes (the node answers
/// that it already has them, or takes them again) and never an outcome, so the risk is on the
/// side of waiting too long, not too short. The owner may change it (`watch_silence_ms`).
///
/// [`MAX_ERA`]: crate::kreivo::MAX_ERA
pub const WATCH_SILENCE: Duration = Duration::from_secs(120);

/// What a watch hears: the stream of a submitted transaction's statuses.
pub trait Statuses: Send + 'static {
    /// One status, as the node reports it.
    type Status: Send;
    /// Why the watch failed.
    type Error: Display + Send;

    /// The next status; `None` when the stream has ended.
    fn next(&mut self) -> impl Future<Output = Option<Result<Self::Status, Self::Error>>> + Send;
}

impl<T, C> Statuses for subxt::tx::TxProgress<T, C>
where
    T: subxt::Config,
    C: subxt::client::OnlineClientT<T> + Send + 'static,
{
    type Status = subxt::tx::TxStatus<T, C>;
    type Error = subxt::Error;

    fn next(&mut self) -> impl Future<Output = Option<Result<Self::Status, Self::Error>>> + Send {
        subxt::tx::TxProgress::next(self)
    }
}

/// The transactions submitted and still watched, by hash.
pub struct Watches {
    silence: Duration,
    tasks: Mutex<HashMap<[u8; 32], JoinHandle<()>>>,
}

impl Watches {
    /// No watch yet; each one is dropped after `silence` without a status.
    pub fn new(silence: Duration) -> Self {
        Watches {
            silence,
            tasks: Mutex::default(),
        }
    }

    /// How long a watch may be silent before it is dropped.
    pub fn silence(&self) -> Duration {
        self.silence
    }

    /// Whether the transaction `hash` is watched now. A watch that has ended is forgotten here.
    pub fn is_watching(&self, hash: &[u8; 32]) -> bool {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|_, w| !w.is_finished());
        tasks.contains_key(hash)
    }

    /// Watches `statuses` for the transaction `hash` until its stream ends, fails, or is silent
    /// for [`Watches::silence`].
    pub fn watch<S: Statuses>(&self, hash: [u8; 32], statuses: S) {
        let silence = self.silence;
        let task = tokio::spawn(follow(hash, statuses, silence));
        self.tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(hash, task);
    }
}

async fn follow<S: Statuses>(hash: [u8; 32], mut statuses: S, silence: Duration) {
    let hash = hex::encode(hash);
    loop {
        match tokio::time::timeout(silence, statuses.next()).await {
            Ok(Some(Ok(s))) => {
                tracing::debug!(%hash, status = ?std::mem::discriminant(&s), "a submitted transaction's status")
            }
            Ok(Some(Err(e))) => {
                tracing::warn!(error = %e, %hash, "a submitted transaction's watch ended");
                return;
            }
            Ok(None) => return,
            // Not an outcome: the watch is dropped and the hash may be submitted again; the
            // transaction's fate is read from the chain, as ever.
            Err(_) => {
                tracing::warn!(
                    %hash,
                    silent_for_ms = u64::try_from(silence.as_millis()).unwrap_or(u64::MAX),
                    "a submitted transaction's watch went silent and was dropped; its outcome is unknown and is looked at again"
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Issue #84: a silent watch is dropped, whatever the peer does, and nothing is concluded from
    //! it. The peer here is a stream that says what the test tells it and then nothing at all.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A bound short enough for a test and long enough that a running test is not mistaken for a
    /// silent one: the tests never depend on being quicker than it.
    const SHORT: Duration = Duration::from_millis(150);

    /// What a test waits for, at most, before it calls a watch stuck.
    const STUCK: Duration = Duration::from_secs(20);

    /// A peer that says nothing, or that sends a status every `gap` for as long as `talking` is
    /// set and then falls silent for good. It marks `released` when it is dropped: the
    /// subscription is let go.
    struct Peer {
        talking: Option<(Arc<AtomicBool>, Duration)>,
        released: Arc<AtomicBool>,
    }

    impl Peer {
        fn silent(released: &Arc<AtomicBool>) -> Self {
            Peer {
                talking: None,
                released: released.clone(),
            }
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    impl Statuses for Peer {
        type Status = u8;
        type Error = String;

        async fn next(&mut self) -> Option<Result<u8, String>> {
            if let Some((talking, gap)) = &self.talking {
                tokio::time::sleep(*gap).await;
                if talking.load(Ordering::SeqCst) {
                    return Some(Ok(0));
                }
            }
            std::future::pending().await
        }
    }

    async fn until_forgotten(watches: &Watches, hash: &[u8; 32]) {
        tokio::time::timeout(STUCK, async {
            while watches.is_watching(hash) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the watch was still held after the silence bound");
    }

    #[tokio::test]
    async fn issue_84_a_watch_that_goes_silent_is_dropped_and_its_hash_is_free_again() {
        let watches = Watches::new(SHORT);
        let hash = [7u8; 32];
        let released = Arc::new(AtomicBool::new(false));
        watches.watch(hash, Peer::silent(&released));
        // While it lives, the hash is skipped by `resend` (one watcher per transaction).
        assert!(watches.is_watching(&hash));

        until_forgotten(&watches, &hash).await;
        // The subscription was let go, and the hash can be watched again, the same bytes.
        assert!(released.load(Ordering::SeqCst));
        let again = Arc::new(AtomicBool::new(false));
        watches.watch(hash, Peer::silent(&again));
        assert!(watches.is_watching(&hash));
        until_forgotten(&watches, &hash).await;
        assert!(again.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn issue_84_the_silence_is_counted_from_the_last_status_not_from_the_start() {
        // Statuses keep coming a twentieth of the bound apart for longer than the whole bound: the
        // watch is not dropped while it is being told things, and is dropped once it falls silent.
        // The peer stops only when the test says so, so a slow runner cannot end the test early.
        let silence = Duration::from_secs(2);
        let watches = Watches::new(silence);
        let hash = [8u8; 32];
        let released = Arc::new(AtomicBool::new(false));
        let talking = Arc::new(AtomicBool::new(true));
        watches.watch(
            hash,
            Peer {
                talking: Some((talking.clone(), silence / 20)),
                released: released.clone(),
            },
        );
        tokio::time::sleep(silence + silence / 2).await;
        assert!(
            watches.is_watching(&hash),
            "a watch that is hearing news was dropped"
        );
        assert!(!released.load(Ordering::SeqCst));

        talking.store(false, Ordering::SeqCst);
        until_forgotten(&watches, &hash).await;
        assert!(released.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn issue_84_a_failed_or_ended_watch_is_still_forgotten() {
        struct Failing;
        impl Statuses for Failing {
            type Status = u8;
            type Error = String;
            async fn next(&mut self) -> Option<Result<u8, String>> {
                Some(Err("the node went away".into()))
            }
        }
        struct Ended;
        impl Statuses for Ended {
            type Status = u8;
            type Error = String;
            async fn next(&mut self) -> Option<Result<u8, String>> {
                None
            }
        }
        let watches = Watches::new(Duration::from_secs(3600));
        watches.watch([1; 32], Failing);
        watches.watch([2; 32], Ended);
        until_forgotten(&watches, &[1; 32]).await;
        until_forgotten(&watches, &[2; 32]).await;
    }

    #[test]
    fn issue_84_the_default_bound_is_two_minutes_and_a_whole_era_is_longer() {
        assert_eq!(WATCH_SILENCE, Duration::from_secs(120));
        // A block is about six seconds: the bound is well inside the era of a transaction.
        assert!(WATCH_SILENCE < Duration::from_secs(6 * crate::kreivo::MAX_ERA));
    }
}
