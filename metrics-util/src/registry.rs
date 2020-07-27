use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use arc_swap::ArcSwap;
use crossbeam_utils::sync::ShardedLock;
use im::HashMap as ImmutableHashMap;
use metrics::Identifier;
use parking_lot::Mutex;

/// A high-performance metric registry.
///
/// `Registry` provides the ability to maintain a central listing of metrics mapped by a given key.
///
/// In many cases, `K` will be a composite key, where the fundamental `Key` type from `metrics` is
/// present, and differentiation is provided by storing the metric type alongside.
///
/// Metrics themselves are represented opaquely behind `H`.  In most cases, this would be a
/// thread-safe handle to the underlying metrics storage that the owner of the registry can use to
/// update the actual metric value(s) as needed.  `Handle`, from this crate, is a solid default
/// choice.
///
/// `Registry` handles deduplicating metrics, and will return the `Identifier` for an existing
/// metric if a caller attempts to reregister it.
///
/// `Registry` is optimized for reads.
pub struct Registry<K, H> {
    mappings: ArcSwap<ImmutableHashMap<K, Identifier>>,
    handles: ShardedLock<Vec<H>>,
    lock: Mutex<()>,
}

impl<K, H> Registry<K, H>
where
    K: Eq + Hash + Clone,
{
    /// Creates a new `Registry`.
    pub fn new() -> Self {
        Registry {
            mappings: ArcSwap::from(Arc::new(ImmutableHashMap::new())),
            handles: ShardedLock::new(Vec::new()),
            lock: Mutex::new(()),
        }
    }

    /// Get or create a new identifier for a given key.
    ///
    /// If the key is not already mapped, a new identifier will be generated, and the given handle
    /// stored along side of it.  If the key is already mapped, its identifier will be returned.
    pub fn get_or_create_identifier<F>(&self, key: K, f: F) -> Identifier
    where
        F: FnOnce(&K) -> H,
    {
        // Check our mapping table first.
        if let Some(id) = self.mappings.load().get(&key) {
            return id.clone();
        }

        // Take control of the registry.
        let guard = self.lock.lock();

        // Check our mapping table again, in case someone just inserted what we need.
        let mappings = self.mappings.load();
        if let Some(id) = mappings.get(&key) {
            return id.clone();
        }

        // Our identifier will be the index we insert the handle into.
        let mut wg = self
            .handles
            .write()
            .expect("handles write lock was poisoned!");
        let id = wg.len().into();
        let handle = f(&key);
        wg.push(handle);
        drop(wg);

        // Update our mapping table and drop the lock.
        let new_mappings = mappings.update(key, id);
        drop(mappings);
        self.mappings.store(Arc::new(new_mappings));
        drop(guard);

        id
    }

    /// Gets the handle for a given identifier.
    pub fn with_handle<F, V>(&self, identifier: Identifier, f: F) -> Option<V>
    where
        F: FnOnce(&H) -> V,
    {
        match identifier {
            Identifier::Valid(idx) => {
                let rg = self
                    .handles
                    .read()
                    .expect("handles read lock was poisoned!");
                rg.get(idx).map(f)
            },
            Identifier::Invalid => None,
        }
    }
}

impl<K, H> Registry<K, H>
where
    K: Eq + Hash + Clone,
    H: Clone,
{
    /// Gets a map of all present handles, mapped by key.
    ///
    /// Handles must implement `Clone`.  This map is a point-in-time snapshot of the registry.
    pub fn get_handles(&self) -> HashMap<K, H> {
        let guard = self.mappings.load();
        let mappings = ImmutableHashMap::clone(&guard);
        let rg = self
            .handles
            .read()
            .expect("handles read lock was poisoned!");
        mappings
            .into_iter()
            .filter_map(|(key, id)| {
                match id {
                    Identifier::Valid(idx) => {
                        let handle = rg.get(idx).expect("handle not present!").clone();
                        Some((key, handle))
                    },
                    Identifier::Invalid => None,
                }
            })
            .collect::<HashMap<_, _>>()
    }
}
