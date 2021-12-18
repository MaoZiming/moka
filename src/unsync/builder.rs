use super::{Cache, Weigher};

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    time::Duration,
};

const YEAR_SECONDS: u64 = 365 * 24 * 3600;

/// Builds a [`Cache`][cache-struct] with various configuration knobs.
///
/// [cache-struct]: ./struct.Cache.html
///
/// # Examples
///
/// ```rust
/// use moka::unsync::Cache;
/// use std::time::Duration;
///
/// let mut cache = Cache::builder()
///     // Max 10,000 elements
///     .max_capacity(10_000)
///     // Time to live (TTL): 30 minutes
///     .time_to_live(Duration::from_secs(30 * 60))
///     // Time to idle (TTI):  5 minutes
///     .time_to_idle(Duration::from_secs( 5 * 60))
///     // Create the cache.
///     .build();
///
/// // This entry will expire after 5 minutes (TTI) if there is no get().
/// cache.insert(0, "zero");
///
/// // This get() will extend the entry life for another 5 minutes.
/// cache.get(&0);
///
/// // Even though we keep calling get(), the entry will expire
/// // after 30 minutes (TTL) from the insert().
/// ```
///
pub struct CacheBuilder<K, V, C> {
    max_capacity: Option<usize>,
    initial_capacity: Option<usize>,
    weigher: Option<Weigher<K, V>>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    cache_type: PhantomData<C>,
}

impl<K, V> Default for CacheBuilder<K, V, Cache<K, V, RandomState>>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self {
            max_capacity: None,
            initial_capacity: None,
            weigher: None,
            time_to_live: None,
            time_to_idle: None,
            cache_type: Default::default(),
        }
    }
}

impl<K, V> CacheBuilder<K, V, Cache<K, V, RandomState>>
where
    K: Eq + Hash,
{
    /// Construct a new `CacheBuilder` that will be used to build a `Cache` holding
    /// up to `max_capacity` entries.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            max_capacity: Some(max_capacity),
            ..Default::default()
        }
    }

    /// Builds a `Cache<K, V>`.
    pub fn build(self) -> Cache<K, V, RandomState> {
        let build_hasher = RandomState::default();
        self.time_to_live.map(|d| {
            if Duration::from_secs(1_000 * YEAR_SECONDS) < d {
                panic!("time_to_live is longer than 1000 years");
            } else {
                d
            }
        });
        self.time_to_idle.map(|d| {
            if Duration::from_secs(1_000 * YEAR_SECONDS) < d {
                panic!("time_to_idle is longer than 1000 years");
            } else {
                d
            }
        });
        Cache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            build_hasher,
            self.weigher,
            self.time_to_live,
            self.time_to_idle,
        )
    }

    /// Builds a `Cache<K, V, S>`, with the given `hasher`.
    pub fn build_with_hasher<S>(self, hasher: S) -> Cache<K, V, S>
    where
        S: BuildHasher + Clone,
    {
        Cache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            hasher,
            self.weigher,
            self.time_to_live,
            self.time_to_idle,
        )
    }
}

impl<K, V, C> CacheBuilder<K, V, C> {
    /// Sets the max capacity of the cache.
    pub fn max_capacity(self, max_capacity: usize) -> Self {
        Self {
            max_capacity: Some(max_capacity),
            ..self
        }
    }

    /// Sets the initial capacity of the cache.
    pub fn initial_capacity(self, capacity: usize) -> Self {
        Self {
            initial_capacity: Some(capacity),
            ..self
        }
    }

    /// Sets the weigher closure of the cache.
    pub fn weigher(self, weigher: impl FnMut(&K, &V) -> u64 + 'static) -> Self {
        Self {
            weigher: Some(Box::new(weigher)),
            ..self
        }
    }

    /// Sets the time to live of the cache.
    ///
    /// A cached entry will be expired after the specified duration past from
    /// `insert`.
    pub fn time_to_live(self, duration: Duration) -> Self {
        Self {
            time_to_live: Some(duration),
            ..self
        }
    }

    /// Sets the time to idle of the cache.
    ///
    /// A cached entry will be expired after the specified duration past from `get`
    /// or `insert`.
    pub fn time_to_idle(self, duration: Duration) -> Self {
        Self {
            time_to_idle: Some(duration),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CacheBuilder;

    use std::time::Duration;

    #[tokio::test]
    async fn build_cache() {
        // Cache<char, String>
        let mut cache = CacheBuilder::new(100).build();

        assert_eq!(cache.max_capacity(), Some(100));
        assert_eq!(cache.time_to_live(), None);
        assert_eq!(cache.time_to_idle(), None);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some(&"Alice"));

        let mut cache = CacheBuilder::new(100)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();

        assert_eq!(cache.max_capacity(), Some(100));
        assert_eq!(cache.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(cache.time_to_idle(), Some(Duration::from_secs(15 * 60)));

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some(&"Alice"));
    }

    #[tokio::test]
    #[should_panic(expected = "time_to_live is longer than 1000 years")]
    async fn build_cache_too_long_ttl() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<char, String, _> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_live(duration + Duration::from_secs(1))
            .build();
    }

    #[tokio::test]
    #[should_panic(expected = "time_to_idle is longer than 1000 years")]
    async fn build_cache_too_long_tti() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<char, String, _> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_idle(duration + Duration::from_secs(1))
            .build();
    }
}
