use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

struct Shared<T: ?Sized> {
    items: Mutex<Vec<(u64, Arc<T>)>>,
    next: AtomicU64,
}

impl<T: ?Sized> Shared<T> {
    fn items(&self) -> MutexGuard<'_, Vec<(u64, Arc<T>)>> {
        // A poisoned lock means a panic mid-update of a plain list; it stays usable.
        self.items.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A place plugins serve things at. Cheap to clone; clones are the same point.
///
/// A point with nothing served lists nothing: that is how "off" reads to
/// whoever uses it, never an error.
pub struct Point<T: ?Sized> {
    shared: Arc<Shared<T>>,
}

impl<T: ?Sized> Clone for Point<T> {
    fn clone(&self) -> Point<T> {
        Point {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T: ?Sized> Default for Point<T> {
    fn default() -> Point<T> {
        Point {
            shared: Arc::new(Shared {
                items: Mutex::new(Vec::new()),
                next: AtomicU64::new(0),
            }),
        }
    }
}

impl<T: ?Sized + Send + Sync + 'static> Point<T> {
    /// An empty point.
    pub fn new() -> Point<T> {
        Point::default()
    }

    /// Serves `item` until the returned [`Registration`] is dropped.
    pub fn serve(&self, item: Arc<T>) -> Registration {
        let id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        self.shared.items().push((id, item));
        let shared = Arc::clone(&self.shared);
        Registration {
            release: Some(Box::new(move || {
                shared.items().retain(|(served, _)| *served != id);
            })),
        }
    }

    /// What is served now, oldest first.
    pub fn all(&self) -> Vec<Arc<T>> {
        self.shared
            .items()
            .iter()
            .map(|(_, item)| Arc::clone(item))
            .collect()
    }
}

/// One item served at a [`Point`]. Dropping it withdraws the item.
#[must_use = "dropping a Registration withdraws what it served"]
pub struct Registration {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}
