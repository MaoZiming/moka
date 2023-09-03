use super::{PredicateId, PredicateIdStr};
use crate::{
    common::{
        concurrent::{AccessTime, KvEntry, ValueEntry},
        time::Instant,
    },
    PredicateError,
};

use parking_lot::{Mutex, MutexGuard};
use std::{
    hash::{BuildHasher, Hash},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use triomphe::Arc as TrioArc;
use uuid::Uuid;

pub(crate) type PredicateFun<K, V> = Arc<dyn Fn(&K, &V) -> bool + Send + Sync + 'static>;

const PREDICATE_MAP_NUM_SEGMENTS: usize = 16;

pub(crate) trait GetOrRemoveEntry<K, V> {
    fn get_value_entry(&self, key: &Arc<K>, hash: u64) -> Option<TrioArc<ValueEntry<K, V>>>;

    fn remove_key_value_if(
        &self,
        key: &Arc<K>,
        hash: u64,
        condition: impl FnMut(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> bool,
    ) -> Option<TrioArc<ValueEntry<K, V>>>
    where
        K: Send + Sync + 'static,
        V: Clone + Send + Sync + 'static;
}

pub(crate) struct KeyDateLite<K> {
    key: Arc<K>,
    hash: u64,
    timestamp: Instant,
}

impl<K> Clone for KeyDateLite<K> {
    fn clone(&self) -> Self {
        Self {
            key: Arc::clone(&self.key),
            hash: self.hash,
            timestamp: self.timestamp,
        }
    }
}

impl<K> KeyDateLite<K> {
    pub(crate) fn new(key: &Arc<K>, hash: u64, timestamp: Instant) -> Self {
        Self {
            key: Arc::clone(key),
            hash,
            timestamp,
        }
    }
}

// pub(crate) struct InvalidationResult<K, V> {
//     pub(crate) invalidated: Vec<KvEntry<K, V>>,
//     pub(crate) is_done: bool,
// }

// impl<K, V> InvalidationResult<K, V> {
//     fn new(invalidated: Vec<KvEntry<K, V>>, is_done: bool) -> Self {
//         Self {
//             invalidated,
//             is_done,
//         }
//     }
// }

pub(crate) struct Invalidator<K, V, S> {
    predicates: crate::cht::SegmentedHashMap<PredicateId, Predicate<K, V>, S>,
    is_empty: AtomicBool,
    scan_context: Arc<ScanContext<K, V>>,
}

// impl<K, V, S> Drop for Invalidator<K, V, S> {
//     fn drop(&mut self) {
//         let ctx = &self.scan_context;
//         // Disallow to create and run a scanning task by now.
//         ctx.is_shutting_down.store(true, Ordering::Release);

//         // Wait for the scanning task to finish. (busy loop)
//         while ctx.is_running.load(Ordering::Acquire) {
//             std::thread::sleep(Duration::from_millis(1));
//         }
//     }
// }

//
// Crate public methods.
//
impl<K, V, S> Invalidator<K, V, S> {
    pub(crate) fn new(hasher: S) -> Self
    where
        S: BuildHasher,
    {
        const CAPACITY: usize = 0;
        let predicates = crate::cht::SegmentedHashMap::with_num_segments_capacity_and_hasher(
            PREDICATE_MAP_NUM_SEGMENTS,
            CAPACITY,
            hasher,
        );
        Self {
            predicates,
            is_empty: AtomicBool::new(true),
            scan_context: Arc::new(ScanContext::default()),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.is_empty.load(Ordering::Acquire)
    }

    pub(crate) fn remove_predicates_registered_before(&self, ts: Instant)
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
        S: BuildHasher,
    {
        let pred_map = &self.predicates;

        let removing_ids = pred_map
            .iter()
            .filter(|(_, pred)| pred.registered_at <= ts)
            .map(|(id, _)| id)
            .collect::<Vec<_>>();

        for id in removing_ids {
            let hash = pred_map.hash(&id);
            pred_map.remove(hash, |k| k == &id);
        }

        if pred_map.is_empty() {
            self.is_empty.store(true, Ordering::Release);
        }
    }

    pub(crate) fn register_predicate(
        &self,
        predicate: PredicateFun<K, V>,
        registered_at: Instant,
    ) -> Result<PredicateId, PredicateError>
    where
        K: Hash + Eq,
        S: BuildHasher,
    {
        const MAX_RETRY: usize = 1_000;
        let mut tries = 0;
        let preds = &self.predicates;

        while tries < MAX_RETRY {
            let id = Uuid::new_v4().as_hyphenated().to_string();

            let hash = preds.hash(&id);
            if preds.contains_key(hash, |k| k == &id) {
                tries += 1;

                continue; // Retry
            }
            let pred = Predicate::new(&id, predicate, registered_at);
            preds.insert_entry_and(id.clone(), hash, pred, |_, _| ());
            self.is_empty.store(false, Ordering::Release);

            return Ok(id);
        }

        // Since we are using 128-bit UUID for the ID and we do retries for MAX_RETRY
        // times, this panic should extremely unlikely occur (unless there is a bug in
        // UUID generation).
        panic!("Cannot assign a new PredicateId to a predicate");
    }

    // This method will be called by the get method of Cache.
    #[inline]
    pub(crate) fn apply_predicates(&self, key: &Arc<K>, entry: &TrioArc<ValueEntry<K, V>>) -> bool
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
        S: BuildHasher,
    {
        if self.is_empty() {
            false
        } else if let Some(ts) = entry.last_modified() {
            Self::do_apply_predicates(
                self.predicates.iter().map(|(_, v)| v),
                key,
                &entry.value,
                ts,
            )
        } else {
            false
        }
    }

    // pub(crate) fn is_task_running(&self) -> bool {
    //     self.scan_context.is_running.load(Ordering::Acquire)
    // }

    pub(crate) fn scan_and_invalidate<C>(
        &self,
        cache: &C,
        candidates: Vec<KeyDateLite<K>>,
        is_truncated: bool,
    ) -> (Vec<KvEntry<K, V>>, bool)
    where
        C: GetOrRemoveEntry<K, V>,
        K: Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
        S: BuildHasher,
    {
        // let ctx = &self.scan_context;

        // // Do not submit a task if this invalidator is about to be dropped.
        // if ctx.is_shutting_down.load(Ordering::Acquire) {
        //     return;
        // }

        // // Ensure there is no pending task and result.
        // assert!(!self.is_task_running());
        // assert!(ctx.result.lock().is_none());

        // // Populate ctx.predicates if it is empty.
        // {
        //     let mut ps = ctx.predicates.lock();
        //     if ps.is_empty() {
        //         *ps = self.predicates.read().values().cloned().collect();
        //     }
        // }

        // self.scan_context.is_running.store(true, Ordering::Release);

        // let task = ScanTask::new(&self.scan_context, candidates, is_truncated);
        // self.thread_pool.pool.execute(move || {
        //     task.execute();
        // });

        let mut predicates = self.scan_context.predicates.lock();
        if predicates.is_empty() {
            *predicates = self.predicates.iter().map(|(_k, v)| v).collect();
        }

        let mut invalidated = Vec::default();
        let mut newest_timestamp = None;

        for candidate in &candidates {
            let key = &candidate.key;
            let hash = candidate.hash;
            let ts = candidate.timestamp;
            if self.apply(&predicates, cache, key, hash, ts) {
                if let Some(entry) = Self::invalidate(cache, key, hash, ts) {
                    invalidated.push(KvEntry {
                        key: Arc::clone(key),
                        entry,
                    })
                }
            }
            newest_timestamp = Some(ts);
        }

        self.remove_finished_predicates(predicates, is_truncated, newest_timestamp);

        (invalidated, self.predicates.is_empty())
    }

    // pub(crate) fn task_result(&self) -> Option<InvalidationResult<K, V>> {
    //     assert!(!self.is_task_running());
    //     let ctx = &self.scan_context;

    //     ctx.result.lock().take().map(|result| {
    //         self.remove_finished_predicates(ctx, &result);
    //         let is_done = ctx.predicates.lock().is_empty();
    //         InvalidationResult::new(result.invalidated, is_done)
    //     })
    // }
}

//
// Private methods.
//
impl<K, V, S> Invalidator<K, V, S> {
    #[inline]
    fn do_apply_predicates<I>(predicates: I, key: &K, value: &V, ts: Instant) -> bool
    where
        I: Iterator<Item = Predicate<K, V>>,
    {
        for predicate in predicates {
            if predicate.is_applicable(ts) && predicate.apply(key, value) {
                return true;
            }
        }
        false
    }

    fn remove_finished_predicates(
        &self,
        mut predicates: MutexGuard<'_, Vec<Predicate<K, V>>>,
        is_truncated: bool,
        newest_timestamp: Option<Instant>,
    ) where
        K: Hash + Eq,
        S: BuildHasher,
    {
        let predicates = &mut *predicates;
        if is_truncated {
            if let Some(ts) = newest_timestamp {
                let (active, finished): (Vec<_>, Vec<_>) =
                    predicates.drain(..).partition(|p| p.is_applicable(ts));

                // Remove finished predicates from the predicate registry.
                self.remove_predicates(&finished);
                // Set the active predicates to the scan context.
                *predicates = active;
            } else {
                unreachable!();
            }
        } else {
            // Remove all the predicates from the predicate registry and scan context.
            self.remove_predicates(predicates);
            predicates.clear();
        }
    }

    fn remove_predicates(&self, predicates: &[Predicate<K, V>])
    where
        K: Hash + Eq,
        S: BuildHasher,
    {
        let pred_map = &self.predicates;
        predicates.iter().for_each(|p| {
            let hash = pred_map.hash(p.id());
            pred_map.remove(hash, |k| k == p.id());
        });

        if pred_map.is_empty() {
            self.is_empty.store(true, Ordering::Release);
        }
    }

    fn apply<C>(
        &self,
        predicates: &[Predicate<K, V>],
        cache: &C,
        key: &Arc<K>,
        hash: u64,
        ts: Instant,
    ) -> bool
    where
        C: GetOrRemoveEntry<K, V>,
    {
        if let Some(entry) = cache.get_value_entry(key, hash) {
            if let Some(lm) = entry.last_modified() {
                if lm == ts {
                    return Invalidator::<_, _, S>::do_apply_predicates(
                        predicates.iter().cloned(),
                        key,
                        &entry.value,
                        lm,
                    );
                }
            }
        }

        false
    }

    fn invalidate<C>(
        cache: &C,
        key: &Arc<K>,
        hash: u64,
        ts: Instant,
    ) -> Option<TrioArc<ValueEntry<K, V>>>
    where
        C: GetOrRemoveEntry<K, V>,
        K: Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        cache.remove_key_value_if(key, hash, |_, v| {
            if let Some(lm) = v.last_modified() {
                lm == ts
            } else {
                false
            }
        })
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S> Invalidator<K, V, S> {
    pub(crate) fn predicate_count(&self) -> usize {
        self.predicates.len()
    }
}

struct ScanContext<K, V> {
    predicates: Mutex<Vec<Predicate<K, V>>>,
}

impl<K, V> Default for ScanContext<K, V> {
    fn default() -> Self {
        Self {
            predicates: Mutex::new(Vec::default()),
        }
    }
}

struct Predicate<K, V> {
    id: PredicateId,
    f: PredicateFun<K, V>,
    registered_at: Instant,
}

impl<K, V> Clone for Predicate<K, V> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            f: Arc::clone(&self.f),
            registered_at: self.registered_at,
        }
    }
}

impl<K, V> Predicate<K, V> {
    fn new(id: PredicateIdStr<'_>, f: PredicateFun<K, V>, registered_at: Instant) -> Self {
        Self {
            id: id.to_string(),
            f,
            registered_at,
        }
    }

    fn id(&self) -> PredicateIdStr<'_> {
        &self.id
    }

    fn is_applicable(&self, last_modified: Instant) -> bool {
        last_modified <= self.registered_at
    }

    fn apply(&self, key: &K, value: &V) -> bool {
        (self.f)(key, value)
    }
}
