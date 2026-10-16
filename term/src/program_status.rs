//! Program status records reported through OSC 7501, the Program Status
//! Protocol: a program tells the terminal whether it is at rest, working,
//! blocked on the user, finished or failed, as structured records instead of
//! leaving the terminal to scrape its screen.
//!
//! Spec (draft): <https://www.superlogical.com/rex/docs/build/program-status>
//!
//! This is a TGZTerminal addition. The parser lives in
//! `wezterm_escape_parser::osc` (`ProgramStatusReport`); this module is the
//! per-terminal store and its lifetime rules.
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
pub use wezterm_escape_parser::osc::{ProgramBlockedKind, ProgramState, ProgramStatusReport};

/// Most records a terminal keeps; when full, the least recently updated one
/// goes. The spec caps it at 256 and requires at least 64.
pub const PROGRAM_STATUS_MAX_RECORDS: usize = 256;

/// One stored record: exactly the keys of the last report for its id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProgramStatusRecord {
    /// Never `ProgramState::Clear`: a clear removes records instead.
    pub state: ProgramState,
    pub kind: Option<ProgramBlockedKind>,
    pub progress: Option<u8>,
    /// As reported; see `ProgramStatus::app` for the inherited value.
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

impl ProgramStatusRecord {
    /// `working` and `blocked` describe a live process: they are dropped
    /// when it exits or a new shell prompt starts. `idle`, `done` and
    /// `error` outlive both.
    pub fn is_live(&self) -> bool {
        matches!(self.state, ProgramState::Working | ProgramState::Blocked)
    }
}

/// The records a terminal holds, keyed by id. The root record's id is the
/// empty string; a child's id is a `/` path (`build/test` is beneath
/// `build`, which is beneath the root).
#[derive(Debug, Default, Clone)]
pub struct ProgramStatus {
    records: BTreeMap<String, (u64, ProgramStatusRecord)>,
    clock: u64,
}

impl PartialEq for ProgramStatus {
    /// Content equality: when a record was last touched does not count.
    fn eq(&self, other: &Self) -> bool {
        self.records.len() == other.records.len()
            && self
                .records
                .iter()
                .zip(other.records.iter())
                .all(|((ka, (_, a)), (kb, (_, b)))| ka == kb && a == b)
    }
}

impl Eq for ProgramStatus {}

impl Hash for ProgramStatus {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for (id, (_, record)) in &self.records {
            id.hash(state);
            record.hash(state);
        }
    }
}

/// Whether `id` is `ancestor` or lies beneath it. The root (`""`) is the
/// ancestor of everything.
fn is_at_or_beneath(id: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || id == ancestor
        || (id.len() > ancestor.len()
            && id.starts_with(ancestor)
            && id.as_bytes()[ancestor.len()] == b'/')
}

impl ProgramStatus {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// The root record, if one was reported.
    pub fn root(&self) -> Option<&ProgramStatusRecord> {
        self.get("")
    }

    pub fn get(&self, id: &str) -> Option<&ProgramStatusRecord> {
        self.records.get(id).map(|(_, record)| record)
    }

    /// Every record in id order, the root (`""`) first.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProgramStatusRecord)> {
        self.records
            .iter()
            .map(|(id, (_, record))| (id.as_str(), record))
    }

    /// Records other than the root.
    pub fn children(&self) -> impl Iterator<Item = (&str, &ProgramStatusRecord)> {
        self.iter().filter(|(id, _)| !id.is_empty())
    }

    /// The `app` that applies to `id`: its own, else its nearest ancestor's.
    pub fn app(&self, id: &str) -> Option<&str> {
        let mut path = id;
        loop {
            if let Some(app) = self.get(path).and_then(|r| r.app.as_deref()) {
                return Some(app);
            }
            if path.is_empty() {
                return None;
            }
            path = path.rfind('/').map(|i| &path[..i]).unwrap_or("");
        }
    }

    /// Apply one report. Returns whether the stored records changed.
    pub fn apply(&mut self, report: ProgramStatusReport) -> bool {
        let id = report.id.unwrap_or_default();
        if report.state == ProgramState::Clear {
            let before = self.records.len();
            self.records.retain(|key, _| !is_at_or_beneath(key, &id));
            return self.records.len() != before;
        }

        let record = ProgramStatusRecord {
            state: report.state,
            kind: report.kind,
            progress: report.progress,
            app: report.app,
            title: report.title,
            msg: report.msg,
        };
        self.clock += 1;
        let stamp = self.clock;
        if let Some((touched, existing)) = self.records.get_mut(&id) {
            *touched = stamp;
            if *existing == record {
                return false;
            }
            *existing = record;
            return true;
        }

        if self.records.len() >= PROGRAM_STATUS_MAX_RECORDS {
            if let Some(oldest) = self
                .records
                .iter()
                .min_by_key(|(_, (touched, _))| *touched)
                .map(|(key, _)| key.clone())
            {
                self.records.remove(&oldest);
            }
        }
        self.records.insert(id, (stamp, record));
        true
    }

    /// The attached process exited or a new shell prompt started (OSC 133
    /// `A`): `working` and `blocked` records no longer describe anything.
    /// Returns whether anything was dropped.
    pub fn drop_live(&mut self) -> bool {
        let before = self.records.len();
        self.records.retain(|_, (_, record)| !record.is_live());
        self.records.len() != before
    }

    /// Full reset (RIS) removes every record. Returns whether any existed.
    pub fn clear(&mut self) -> bool {
        let had_any = !self.records.is_empty();
        self.records.clear();
        had_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(state: ProgramState, id: Option<&str>) -> ProgramStatusReport {
        ProgramStatusReport {
            state,
            id: id.map(str::to_string),
            kind: None,
            progress: None,
            app: None,
            title: None,
            msg: None,
        }
    }

    #[test]
    fn a_report_replaces_its_record_wholesale() {
        let mut status = ProgramStatus::default();
        let mut first = report(ProgramState::Working, None);
        first.app = Some("cargo".into());
        first.title = Some("build".into());
        assert!(status.apply(first.clone()));
        assert!(!status.apply(first), "an identical report changes nothing");

        assert!(status.apply(report(ProgramState::Idle, None)));
        let root = status.root().unwrap();
        assert_eq!(root.state, ProgramState::Idle);
        assert_eq!(root.app, None, "keys left out of a report are removed");
        assert_eq!(root.title, None);
    }

    #[test]
    fn clear_removes_the_record_and_everything_beneath_it() {
        let mut status = ProgramStatus::default();
        for id in [None, Some("build"), Some("build/test"), Some("builder")] {
            status.apply(report(ProgramState::Working, id));
        }
        assert!(status.apply(report(ProgramState::Clear, Some("build"))));
        let ids: Vec<&str> = status.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec!["", "builder"], "a sibling prefix is not a child");

        assert!(status.apply(report(ProgramState::Clear, None)));
        assert!(
            status.is_empty(),
            "a clear without an id removes everything"
        );
        assert!(!status.apply(report(ProgramState::Clear, None)));
    }

    #[test]
    fn app_is_inherited_from_the_nearest_ancestor() {
        let mut status = ProgramStatus::default();
        let mut root = report(ProgramState::Idle, None);
        root.app = Some("claude-code".into());
        status.apply(root);
        status.apply(report(ProgramState::Working, Some("agent/step")));
        assert_eq!(status.app("agent/step"), Some("claude-code"));
        assert_eq!(status.app("missing"), Some("claude-code"));
    }

    #[test]
    fn a_prompt_drops_live_records_only() {
        let mut status = ProgramStatus::default();
        status.apply(report(ProgramState::Working, None));
        status.apply(report(ProgramState::Blocked, Some("a")));
        status.apply(report(ProgramState::Done, Some("b")));
        status.apply(report(ProgramState::Error, Some("c")));
        status.apply(report(ProgramState::Idle, Some("d")));
        assert!(status.drop_live());
        let ids: Vec<&str> = status.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec!["b", "c", "d"]);
        assert!(!status.drop_live());
    }

    #[test]
    fn a_full_store_evicts_the_least_recently_updated_record() {
        let mut status = ProgramStatus::default();
        for i in 0..PROGRAM_STATUS_MAX_RECORDS {
            let id = format!("r{i}");
            status.apply(report(ProgramState::Working, Some(&id)));
        }
        // Touch the oldest so the second-oldest becomes the victim.
        status.apply(report(ProgramState::Idle, Some("r0")));
        status.apply(report(ProgramState::Working, Some("new")));
        assert_eq!(status.len(), PROGRAM_STATUS_MAX_RECORDS);
        assert!(status.get("r0").is_some());
        assert!(status.get("r1").is_none());
        assert!(status.get("new").is_some());
    }

    #[test]
    fn equality_and_hash_ignore_when_a_record_was_touched() {
        use std::collections::hash_map::DefaultHasher;
        let mut a = ProgramStatus::default();
        let mut b = ProgramStatus::default();
        a.apply(report(ProgramState::Working, None));
        b.apply(report(ProgramState::Idle, None));
        b.apply(report(ProgramState::Working, None));
        assert_eq!(a, b);
        let hash = |s: &ProgramStatus| {
            let mut h = DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&a), hash(&b));
    }
}
