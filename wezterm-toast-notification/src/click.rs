//! Click handlers parked for later.
//!
//! Most backends own the `ToastNotification` for the whole life of the
//! notification and can hold the handler directly in their event closure. macOS
//! cannot: the click arrives at a single process-wide delegate that is handed
//! nothing but a `UNNotificationResponse`, so the handler has to be looked up by
//! the notification's identifier. This registry is that lookup.
//!
//! Bounded on purpose. A persistent notification (`timeout: None`) is freed only
//! when the user clicks or dismisses it, and a user who does neither would
//! otherwise leak a handler per notification for the life of the process.

use crate::ToastClick;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many un-clicked handlers to keep. Evicting one only costs a click on a
/// very old banner doing nothing.
pub(crate) const MAX_PENDING: usize = 64;
/// Nobody comes back to a banner an hour later.
pub(crate) const TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub(crate) struct ClickRegistry {
    by_id: HashMap<String, (Instant, ToastClick)>,
    /// Insertion order, so eviction is oldest-first without scanning.
    order: VecDeque<String>,
}

impl ClickRegistry {
    pub(crate) fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(crate) fn insert(&mut self, id: String, handler: ToastClick, now: Instant) {
        self.evict(now);
        if self.by_id.insert(id.clone(), (now, handler)).is_none() {
            self.order.push_back(id);
        }
    }

    /// Remove and return the handler. Taking rather than borrowing is what makes
    /// a click fire exactly once: a dismiss arriving after an activation finds
    /// nothing left.
    pub(crate) fn take(&mut self, id: &str) -> Option<ToastClick> {
        let entry = self.by_id.remove(id)?;
        self.order.retain(|candidate| candidate != id);
        Some(entry.1)
    }

    pub(crate) fn forget(&mut self, id: &str) {
        if self.by_id.remove(id).is_some() {
            self.order.retain(|candidate| candidate != id);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_id.len()
    }

    fn evict(&mut self, now: Instant) {
        let by_id = &mut self.by_id;
        self.order.retain(|id| {
            let expired = by_id
                .get(id)
                .map(|(at, _)| now.duration_since(*at) >= TTL)
                .unwrap_or(true);
            if expired {
                by_id.remove(id);
            }
            !expired
        });

        while self.by_id.len() >= MAX_PENDING {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.by_id.remove(&oldest);
                }
                None => break,
            }
        }
    }
}

static REGISTRY: Mutex<Option<ClickRegistry>> = Mutex::new(None);

/// A handler that panics must not take notifications down with it for the rest
/// of the session, so the poison is deliberately ignored.
pub(crate) fn with<R>(func: impl FnOnce(&mut ClickRegistry) -> R) -> R {
    let mut guard = REGISTRY.lock().unwrap_or_else(|err| err.into_inner());
    func(guard.get_or_insert_with(ClickRegistry::new))
}

/// What a notification response means. Only the delegate needs this, but it is
/// pure, so it lives here where it can be tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseKind {
    /// The user asked to be taken to whatever produced the notification.
    Activate,
    /// Swiped away. Never focus anything.
    Dismiss,
}

/// Any response that is not an explicit dismiss counts as an activation:
/// clicking the banner body, one of our action buttons, or anything a future
/// macOS adds. Failing the other way would silently break the feature.
pub(crate) fn classify_action(action_id: &str, dismiss_id: &str) -> ResponseKind {
    if action_id == dismiss_id {
        ResponseKind::Dismiss
    } else {
        ResponseKind::Activate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn counting_handler() -> (ToastClick, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let handle = Arc::clone(&count);
        let handler: ToastClick = Arc::new(move || {
            handle.fetch_add(1, Ordering::SeqCst);
        });
        (handler, count)
    }

    #[test]
    fn take_fires_once() {
        let mut registry = ClickRegistry::new();
        let (handler, count) = counting_handler();
        let now = Instant::now();
        registry.insert("a".to_string(), handler, now);

        registry.take("a").expect("handler is parked")();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // A dismiss arriving after the click must find nothing.
        assert!(registry.take("a").is_none());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn unknown_id_is_a_no_op() {
        let mut registry = ClickRegistry::new();
        assert!(registry.take("nope").is_none());
        registry.forget("nope");
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn forget_drops_without_firing() {
        let mut registry = ClickRegistry::new();
        let (handler, count) = counting_handler();
        registry.insert("a".to_string(), handler, Instant::now());
        registry.forget("a");
        assert!(registry.take("a").is_none());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn eviction_keeps_the_newest() {
        let mut registry = ClickRegistry::new();
        let now = Instant::now();
        for idx in 0..MAX_PENDING + 8 {
            let (handler, _) = counting_handler();
            registry.insert(format!("id-{idx}"), handler, now);
        }
        assert!(registry.len() < MAX_PENDING + 8);
        // The oldest are gone, the newest survives.
        assert!(registry.take("id-0").is_none());
        assert!(registry.take(&format!("id-{}", MAX_PENDING + 7)).is_some());
    }

    #[test]
    fn stale_entries_expire() {
        let mut registry = ClickRegistry::new();
        let (handler, _) = counting_handler();
        let long_ago = Instant::now();
        registry.insert("old".to_string(), handler, long_ago);

        let (handler, _) = counting_handler();
        registry.insert("new".to_string(), handler, long_ago + TTL);

        assert!(registry.take("old").is_none());
        assert!(registry.take("new").is_some());
    }

    #[test]
    fn only_the_dismiss_id_is_a_dismiss() {
        assert_eq!(
            classify_action(
                "com.apple.UNNotificationDismissActionIdentifier",
                "com.apple.UNNotificationDismissActionIdentifier"
            ),
            ResponseKind::Dismiss
        );
        assert_eq!(
            classify_action(
                "com.apple.UNNotificationDefaultActionIdentifier",
                "com.apple.UNNotificationDismissActionIdentifier"
            ),
            ResponseKind::Activate
        );
        assert_eq!(
            classify_action(
                "SHOW_URL",
                "com.apple.UNNotificationDismissActionIdentifier"
            ),
            ResponseKind::Activate
        );
        assert_eq!(
            classify_action(
                "something.apple.invents.later",
                "com.apple.UNNotificationDismissActionIdentifier"
            ),
            ResponseKind::Activate
        );
    }
}
