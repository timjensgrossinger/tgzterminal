//! Fan-out: one task split into parts that run as child flights, wave by
//! wave. Everything here is pure: the plan's shape, its checks, the wave
//! order, the barrier between waves, the heuristic split when no planner
//! answers, and the one combined result. The engine owns the flights.
//!
//! Ported from Threnody's planner (`shared/planner.py`: `build_waves`,
//! `validate_topology`, `evaluate_fanout`) and `shared/heuristic_plan.py`
//! (one agent per file, same-folder coupling, packing to a cap), stricter
//! where the input comes from an agent: an unknown dependency, a cycle or
//! a path outside the folder is an error, not a warning.

use crate::router::{task_files, Tier};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use tower_proto::{Fanout, FanoutTask, TaskState};

/// How big a fan-out may get.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Parts running at once.
    pub max_agents: u32,
    pub max_subtasks: u32,
    pub max_waves: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_agents: 4,
            max_subtasks: 8,
            max_waves: 4,
        }
    }
}

/// Read a plan from the `tower_fanout` arguments or a planner's JSON:
/// `{analysis?, subtasks: [...]}` or the list alone. Ids may be numbers,
/// `description` means `brief`, `target_file` means `target_files`.
pub fn parse_plan(v: &Value) -> Result<(Vec<FanoutTask>, String), String> {
    let (list, analysis) = match v {
        Value::Array(a) => (a, String::new()),
        Value::Object(o) => {
            let list = o
                .get("subtasks")
                .or_else(|| o.get("tasks"))
                .and_then(Value::as_array)
                .ok_or("the plan needs a `subtasks` list")?;
            let analysis = o
                .get("analysis")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            (list, analysis)
        }
        _ => return Err("the plan must be an object with `subtasks` or a list".into()),
    };
    let mut tasks = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let o = item
            .as_object()
            .ok_or_else(|| format!("subtask {} is not an object", i + 1))?;
        // A bare number names `s<n>`, written as a number or a string, the
        // same way `depends_on` reads it.
        let id = match o.get("id") {
            Some(Value::String(s)) if !s.trim().is_empty() => normal_id(s.trim()),
            Some(Value::Number(n)) => format!("s{n}"),
            _ => format!("s{}", i + 1),
        };
        let brief = str_of(o, &["brief", "description", "task"]);
        let title = {
            let t = str_of(o, &["title", "name"]);
            if t.is_empty() {
                brief.lines().next().unwrap_or("").trim().to_string()
            } else {
                t
            }
        };
        let tier = o
            .get("tier")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let depends_on = ids_of(o.get("depends_on").or_else(|| o.get("after")));
        let target_files = strs_of(o.get("target_files").or_else(|| o.get("target_file")));
        let acceptance = strs_of(o.get("acceptance"));
        let read_only = o.get("read_only").and_then(Value::as_bool).unwrap_or(false);
        let role = o
            .get("role")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        tasks.push(FanoutTask {
            id,
            title,
            brief,
            tier,
            depends_on,
            target_files,
            acceptance,
            read_only,
            role,
            ..Default::default()
        });
    }
    Ok((tasks, analysis))
}

fn str_of(o: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| o.get(*k).and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn strs_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => vec![],
    }
}

fn ids_of(v: Option<&Value>) -> Vec<String> {
    strs_of(v).iter().map(|s| normal_id(s)).collect()
}

/// `3` and `"3"` are part `s3`; any other id stays as written.
fn normal_id(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        format!("s{s}")
    } else {
        s.to_string()
    }
}

/// A relative path inside the flight's folder: no root, no `..`, no home.
pub fn is_safe_relative_path(p: &str) -> bool {
    let p = p.trim();
    if p.is_empty()
        || p.starts_with('/')
        || p.starts_with('\\')
        || p.starts_with('~')
        || p.contains(":\\")
        || p.starts_with("//")
    {
        return false;
    }
    // `C:/Windows`, `c:foo`: a drive, not a path inside the folder.
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return false;
    }
    !p.split(|c| c == '/' || c == '\\').any(|seg| seg == "..")
}

/// Everything that makes a plan unrunnable, all at once.
pub fn validate(tasks: &[FanoutTask], limits: &Limits) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();
    if tasks.is_empty() {
        errs.push("the plan has no subtasks".into());
    }
    if tasks.len() as u32 > limits.max_subtasks {
        errs.push(format!(
            "{} subtasks; at most {} are allowed",
            tasks.len(),
            limits.max_subtasks
        ));
    }
    let mut ids = BTreeSet::new();
    for t in tasks {
        if t.id.trim().is_empty() {
            errs.push("a subtask has no id".into());
        } else if !ids.insert(t.id.as_str()) {
            errs.push(format!("subtask id {:?} is used twice", t.id));
        }
        if t.title.trim().is_empty() && t.brief.trim().is_empty() {
            errs.push(format!("subtask {} has neither a title nor a brief", t.id));
        }
        if let Some(tier) = &t.tier {
            if Tier::parse(tier).is_none() {
                errs.push(format!(
                    "subtask {}: tier {tier:?} is not low, medium or high",
                    t.id
                ));
            }
        }
        for f in &t.target_files {
            if !is_safe_relative_path(f) {
                errs.push(format!(
                    "subtask {}: {f:?} is not a relative path inside the folder",
                    t.id
                ));
            }
        }
    }
    for t in tasks {
        for d in &t.depends_on {
            if d == &t.id {
                errs.push(format!("subtask {} depends on itself", t.id));
            } else if !ids.contains(d.as_str()) {
                errs.push(format!("subtask {} depends on unknown {d:?}", t.id));
            }
        }
    }
    if errs.is_empty() {
        if let Some(cycle) = find_cycle(tasks) {
            errs.push(format!("dependency cycle: {}", cycle.join(" → ")));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// One dependency cycle, as the ids along it, if there is any.
fn find_cycle(tasks: &[FanoutTask]) -> Option<Vec<String>> {
    let deps: BTreeMap<&str, &[String]> = tasks
        .iter()
        .map(|t| (t.id.as_str(), t.depends_on.as_slice()))
        .collect();
    fn walk<'a>(
        id: &'a str,
        deps: &BTreeMap<&'a str, &'a [String]>,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(pos) = path.iter().position(|p| *p == id) {
            let mut cycle: Vec<String> = path[pos..].iter().map(|s| s.to_string()).collect();
            cycle.push(id.to_string());
            return Some(cycle);
        }
        if done.contains(id) {
            return None;
        }
        path.push(id);
        if let Some(ds) = deps.get(id) {
            for d in ds.iter() {
                if let Some(c) = walk(d.as_str(), deps, path, done) {
                    return Some(c);
                }
            }
        }
        path.pop();
        done.insert(id);
        None
    }
    let mut done = BTreeSet::new();
    for t in tasks {
        let mut path = Vec::new();
        if let Some(c) = walk(t.id.as_str(), &deps, &mut path, &mut done) {
            return Some(c);
        }
    }
    None
}

/// Does `a` depend on `b`, directly or through other tasks?
fn depends_on(tasks: &[FanoutTask], a: &str, b: &str) -> bool {
    let mut stack = vec![a];
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(t) = tasks.iter().find(|t| t.id == id) {
            for d in &t.depends_on {
                if d == b {
                    return true;
                }
                stack.push(d.as_str());
            }
        }
    }
    false
}

/// Two parts that both write the same file must not run at once: the later
/// one (in plan order) waits for the earlier one. Children share the
/// flight's folder, so this is what keeps them off each other's edits.
pub fn serialize_conflicts(tasks: &mut [FanoutTask]) {
    let n = tasks.len();
    for j in 0..n {
        for i in 0..j {
            if tasks[i].read_only || tasks[j].read_only {
                continue;
            }
            let shared = tasks[i]
                .target_files
                .iter()
                .any(|f| tasks[j].target_files.iter().any(|g| same_path(f, g)));
            if !shared {
                continue;
            }
            let (a, b) = (tasks[i].id.clone(), tasks[j].id.clone());
            if depends_on(tasks, &b, &a) || depends_on(tasks, &a, &b) {
                continue;
            }
            tasks[j].depends_on.push(a);
        }
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.trim().trim_start_matches("./").replace('\\', "/");
    norm(a) == norm(b)
}

/// Kahn layering: wave 0 is every task with no dependency, wave 1 every
/// task whose dependencies are all in wave 0, and so on. Stable in plan
/// order. A cycle (which `validate` refuses) lands in one last wave.
pub fn build_waves(tasks: &[FanoutTask]) -> Vec<Vec<String>> {
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    let mut waves: Vec<Vec<String>> = Vec::new();
    loop {
        let wave: Vec<String> = tasks
            .iter()
            .filter(|t| !placed.contains(t.id.as_str()))
            .filter(|t| t.depends_on.iter().all(|d| placed.contains(d.as_str())))
            .map(|t| t.id.clone())
            .collect();
        if wave.is_empty() {
            break;
        }
        for id in &wave {
            if let Some(t) = tasks.iter().find(|t| &t.id == id) {
                placed.insert(t.id.as_str());
            }
        }
        waves.push(wave);
    }
    let rest: Vec<String> = tasks
        .iter()
        .filter(|t| !placed.contains(t.id.as_str()))
        .map(|t| t.id.clone())
        .collect();
    if !rest.is_empty() {
        waves.push(rest);
    }
    waves
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// One part: not worth a fan-out, run it as one handoff.
    Single,
    Fanout,
    TooBig(String),
}

/// Is this plan a fan-out at all, and is it within the limits?
pub fn evaluate(tasks: &[FanoutTask], waves: &[Vec<String>], limits: &Limits) -> Verdict {
    if tasks.len() <= 1 {
        return Verdict::Single;
    }
    if waves.len() as u32 > limits.max_waves {
        return Verdict::TooBig(format!(
            "{} waves of dependent work; at most {} are allowed, so the parts are not independent enough",
            waves.len(),
            limits.max_waves
        ));
    }
    Verdict::Fanout
}

/// The split with no planner: one part per file the task names, files in
/// one folder with different stems coupled into one part, merged down to
/// `max_agents` parts. Fewer than two files gives one part with the whole
/// brief (which `evaluate` calls `Single`).
pub fn heuristic_plan(title: &str, brief: &str, max_agents: u32) -> Vec<FanoutTask> {
    let text = format!("{title}\n{brief}");
    let files: Vec<String> = task_files(&text)
        .into_iter()
        .filter(|f| is_safe_relative_path(f))
        .collect();
    let brief = brief.trim();
    if files.len() < 2 {
        return vec![FanoutTask {
            id: "s1".into(),
            title: title.trim().to_string(),
            brief: brief.to_string(),
            target_files: files,
            ..Default::default()
        }];
    }
    let mut groups = coupled_groups(&files);
    pack_to_cap(&mut groups, max_agents.max(1) as usize);
    groups
        .into_iter()
        .enumerate()
        .map(|(i, files)| FanoutTask {
            id: format!("s{}", i + 1),
            title: format!("{}: {}", title.trim(), files.join(", ")),
            brief: format!(
                "{brief}\n\nYour part of this task is the following file(s): {}. Change only those.",
                files.join(", ")
            ),
            target_files: files,
            ..Default::default()
        })
        .collect()
}

fn parent_dir(f: &str) -> String {
    match f.rsplit_once('/') {
        Some((d, _)) => d.to_string(),
        None => String::new(),
    }
}

fn stem_sans_digits(f: &str) -> String {
    let name = f.rsplit('/').next().unwrap_or(f);
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    stem.chars().filter(|c| !c.is_ascii_digit()).collect()
}

/// Files in one folder with at least two distinct (digit-stripped) stems
/// belong together: a module and its neighbour. Replicated files
/// (`greet1.py`, `greet2.py`) are independent and stay apart.
fn coupled_groups(files: &[String]) -> Vec<Vec<String>> {
    let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for f in files {
        let d = parent_dir(f);
        if !by_dir.contains_key(&d) {
            order.push(d.clone());
        }
        by_dir.entry(d).or_default().push(f.clone());
    }
    let mut groups = Vec::new();
    for d in order {
        let fs = by_dir.remove(&d).unwrap_or_default();
        let stems: BTreeSet<String> = fs.iter().map(|f| stem_sans_digits(f)).collect();
        if fs.len() >= 2 && stems.len() >= 2 {
            groups.push(fs);
        } else {
            for f in fs {
                groups.push(vec![f]);
            }
        }
    }
    groups
}

/// Merge the two smallest groups until there are at most `cap`; no file is
/// ever dropped.
fn pack_to_cap(groups: &mut Vec<Vec<String>>, cap: usize) {
    while groups.len() > cap && groups.len() >= 2 {
        let mut idx: Vec<usize> = (0..groups.len()).collect();
        idx.sort_by_key(|&i| groups[i].len());
        let (a, b) = (idx[0].min(idx[1]), idx[0].max(idx[1]));
        let moved = groups.remove(b);
        groups[a].extend(moved);
    }
}

/// What the engine should do next with a fan-out.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Start the pending tasks of this wave.
    StartWave(usize),
    /// Parts are still running or queued.
    Wait,
    /// The current wave is over and these parts failed.
    AskFailure(Vec<String>),
    Finish,
}

/// The barrier: look at the wave in progress and say what comes next.
pub fn next_step(f: &Fanout) -> Step {
    let Some(wave) = f.waves.get(f.wave as usize) else {
        return Step::Finish;
    };
    let states: Vec<TaskState> = wave
        .iter()
        .filter_map(|id| f.task(id))
        .map(|t| t.state)
        .collect();
    if states.iter().any(|s| *s == TaskState::Pending) {
        return Step::StartWave(f.wave as usize);
    }
    if states
        .iter()
        .any(|s| matches!(s, TaskState::Queued | TaskState::Running))
    {
        return Step::Wait;
    }
    let failed: Vec<String> = wave
        .iter()
        .filter(|id| f.task(id).map(|t| t.state) == Some(TaskState::Failed))
        .cloned()
        .collect();
    if !failed.is_empty() {
        return Step::AskFailure(failed);
    }
    if (f.wave as usize) + 1 < f.waves.len() {
        Step::StartWave(f.wave as usize + 1)
    } else {
        Step::Finish
    }
}

/// Parts that wait on a failed or skipped part cannot run: mark them, and
/// what waits on them, skipped. Returns the ids newly skipped.
pub fn skip_dependents(f: &mut Fanout) -> Vec<String> {
    let mut newly = Vec::new();
    loop {
        let blocked: BTreeSet<String> = f
            .subtasks
            .iter()
            .filter(|t| matches!(t.state, TaskState::Failed | TaskState::Skipped))
            .map(|t| t.id.clone())
            .collect();
        let mut changed = false;
        for t in f.subtasks.iter_mut() {
            if t.state == TaskState::Pending && t.depends_on.iter().any(|d| blocked.contains(d)) {
                let why = t
                    .depends_on
                    .iter()
                    .filter(|d| blocked.contains(*d))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                t.state = TaskState::Skipped;
                t.reason = Some(format!("{why} did not return"));
                newly.push(t.id.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    newly
}

/// A part's result as the engine collected it.
pub struct PartResult<'a> {
    pub task_id: &'a str,
    /// The child flight's key (`TWR-5.1`).
    pub key: &'a str,
    /// Who ran it (`Subtask · OpenCode`, `Auto (claude-haiku)`).
    pub by: &'a str,
    pub text: &'a str,
}

/// The one message the asker gets: a header with the counts, then every
/// part's summary (each cut to its share of `max_chars`), then the files
/// touched, then what to do about failures.
pub fn aggregate(f: &Fanout, results: &[PartResult<'_>], max_chars: usize) -> String {
    let returned = f.count(TaskState::Returned);
    let failed = f.count(TaskState::Failed);
    let skipped = f.count(TaskState::Skipped);
    let mut out = format!(
        "Fan-out {}: {returned} of {} parts returned, {failed} failed, {skipped} skipped ({} wave{}).\n",
        f.id,
        f.subtasks.len(),
        f.waves.len(),
        if f.waves.len() == 1 { "" } else { "s" }
    );
    if !f.analysis.trim().is_empty() {
        out.push_str(&format!("Plan: {}\n", f.analysis.trim()));
    }
    let share = max_chars / f.subtasks.len().max(1);
    let mut files: Vec<&str> = Vec::new();
    for t in &f.subtasks {
        for file in &t.target_files {
            if !files.contains(&file.as_str()) {
                files.push(file);
            }
        }
        let r = results.iter().find(|r| r.task_id == t.id);
        let key = r.map(|r| r.key).unwrap_or("");
        let by = r.map(|r| r.by).unwrap_or("");
        let head: Vec<&str> = [t.id.as_str(), key, by]
            .iter()
            .copied()
            .filter(|s| !s.is_empty())
            .collect();
        match t.state {
            TaskState::Returned => {
                out.push_str(&format!("\n## {} · returned\n", head.join(" · ")));
                let text = r.map(|r| r.text.trim()).unwrap_or("");
                out.push_str(&cut(text, share));
                out.push('\n');
            }
            TaskState::Failed | TaskState::Skipped => {
                out.push_str(&format!(
                    "\n## {} · {}: {}\n",
                    head.join(" · "),
                    t.state.label(),
                    t.reason.as_deref().unwrap_or("no result")
                ));
            }
            _ => {
                out.push_str(&format!(
                    "\n## {} · {}\n",
                    head.join(" · "),
                    t.state.label()
                ));
            }
        }
    }
    if !files.is_empty() {
        out.push_str(&format!(
            "\nFiles owned by the parts: {}\n",
            files.join(", ")
        ));
    }
    if failed + skipped > 0 {
        out.push_str(
            "\nFailed and skipped parts were not redone: review the combined changes and finish them yourself, or hand them off again.\n",
        );
    } else {
        out.push_str("\nReview the combined changes before calling the task done.\n");
    }
    out
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(id: &str, deps: &[&str]) -> FanoutTask {
        FanoutTask {
            id: id.into(),
            title: format!("do {id}"),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn with_files(mut task: FanoutTask, files: &[&str]) -> FanoutTask {
        task.target_files = files.iter().map(|s| s.to_string()).collect();
        task
    }

    #[test]
    fn waves_layer_a_diamond_a_chain_and_independent_parts() {
        let diamond = vec![
            t("a", &[]),
            t("b", &["a"]),
            t("c", &["a"]),
            t("d", &["b", "c"]),
        ];
        assert_eq!(
            build_waves(&diamond),
            vec![vec!["a"], vec!["b", "c"], vec!["d"]]
        );
        let chain = vec![t("a", &[]), t("b", &["a"]), t("c", &["b"])];
        assert_eq!(build_waves(&chain), vec![vec!["a"], vec!["b"], vec!["c"]]);
        let flat = vec![t("x", &[]), t("y", &[]), t("z", &[])];
        assert_eq!(build_waves(&flat), vec![vec!["x", "y", "z"]]);
        // Plan order within a wave is kept.
        let order = vec![t("z", &[]), t("a", &[])];
        assert_eq!(build_waves(&order), vec![vec!["z", "a"]]);
    }

    #[test]
    fn validate_rejects_what_an_agent_may_get_wrong() {
        let limits = Limits::default();
        let err = |tasks: Vec<FanoutTask>| validate(&tasks, &limits).unwrap_err().join("; ");
        assert!(err(vec![]).contains("no subtasks"));
        assert!(err(vec![t("a", &["a"])]).contains("depends on itself"));
        assert!(err(vec![t("a", &["nope"])]).contains("unknown \"nope\""));
        assert!(err(vec![t("a", &["b"]), t("b", &["a"])]).contains("cycle"));
        assert!(err(vec![t("a", &[]), t("a", &[])]).contains("used twice"));
        let mut bad_tier = t("a", &[]);
        bad_tier.tier = Some("huge".into());
        assert!(err(vec![bad_tier]).contains("not low, medium or high"));
        assert!(err(vec![with_files(t("a", &[]), &["../etc/passwd"])]).contains("relative path"));
        assert!(err(vec![with_files(t("a", &[]), &["/etc/passwd"])]).contains("relative path"));
        assert!(err(vec![with_files(t("a", &[]), &["~/x"])]).contains("relative path"));
        assert!(err(vec![with_files(t("a", &[]), &["C:/Windows/x"])]).contains("relative path"));
        assert!(err(vec![with_files(t("a", &[]), &["c:x"])]).contains("relative path"));
        assert!(is_safe_relative_path("src/c:x.rs"), "a colon later is fine");
        let many: Vec<FanoutTask> = (0..9).map(|i| t(&format!("s{i}"), &[])).collect();
        assert!(err(many).contains("at most 8"));
        let mut blank = t("a", &[]);
        blank.title.clear();
        assert!(err(vec![blank]).contains("neither a title nor a brief"));
        assert!(validate(
            &[
                with_files(t("a", &[]), &["src/x.rs", "./y.rs"]),
                t("b", &["a"])
            ],
            &limits
        )
        .is_ok());
    }

    #[test]
    fn write_conflicts_are_serialized_read_only_parts_are_not() {
        let mut tasks = vec![
            with_files(t("a", &[]), &["src/lib.rs"]),
            with_files(t("b", &[]), &["src/lib.rs", "src/b.rs"]),
            with_files(t("c", &[]), &["./src/lib.rs"]),
        ];
        tasks[2].read_only = true;
        serialize_conflicts(&mut tasks);
        assert_eq!(tasks[1].depends_on, vec!["a"]);
        assert!(
            tasks[2].depends_on.is_empty(),
            "read-only parts run alongside"
        );
        assert_eq!(build_waves(&tasks), vec![vec!["a", "c"], vec!["b"]]);
        // Already ordered the other way round: no edge is added.
        let mut ordered = vec![
            with_files(t("a", &["b"]), &["x.rs"]),
            with_files(t("b", &[]), &["x.rs"]),
        ];
        serialize_conflicts(&mut ordered);
        assert_eq!(ordered[1].depends_on, Vec::<String>::new());
        assert!(validate(&ordered, &Limits::default()).is_ok());
    }

    #[test]
    fn evaluate_knows_single_and_too_big() {
        let limits = Limits {
            max_waves: 2,
            ..Limits::default()
        };
        let one = vec![t("a", &[])];
        assert_eq!(evaluate(&one, &build_waves(&one), &limits), Verdict::Single);
        let chain = vec![t("a", &[]), t("b", &["a"]), t("c", &["b"])];
        assert!(matches!(
            evaluate(&chain, &build_waves(&chain), &limits),
            Verdict::TooBig(_)
        ));
        let flat = vec![t("a", &[]), t("b", &[])];
        assert_eq!(
            evaluate(&flat, &build_waves(&flat), &limits),
            Verdict::Fanout
        );
    }

    #[test]
    fn heuristic_plan_splits_by_file_couples_a_folder_and_packs_to_the_cap() {
        let plan = heuristic_plan("Fix it", "Edit src/a.rs and lib/b.rs, then docs/c.md.", 4);
        let files: Vec<Vec<String>> = plan.iter().map(|t| t.target_files.clone()).collect();
        assert_eq!(
            files,
            vec![vec!["src/a.rs"], vec!["lib/b.rs"]],
            "md is not a work file"
        );
        assert!(plan[0].brief.contains("Change only those"));
        assert_eq!(plan[0].id, "s1");
        // Two different modules in one folder are one part.
        let plan = heuristic_plan(
            "x",
            "Touch api/users.py and api/orders.py and web/app.ts",
            4,
        );
        let files: Vec<Vec<String>> = plan.iter().map(|t| t.target_files.clone()).collect();
        assert_eq!(
            files,
            vec![vec!["api/users.py", "api/orders.py"], vec!["web/app.ts"]]
        );
        // Replicated stems are independent parts.
        let plan = heuristic_plan("x", "greet1.py greet2.py greet3.py", 4);
        assert_eq!(plan.len(), 3);
        // Different modules in one folder are one part.
        assert_eq!(heuristic_plan("x", "a.rs b.rs c.rs", 4).len(), 1);
        // Packing keeps every file.
        let plan = heuristic_plan("x", "a/x.rs b/x.rs c/x.rs d/x.rs e/x.rs f/x.rs", 2);
        assert_eq!(plan.len(), 2);
        let all: usize = plan.iter().map(|t| t.target_files.len()).sum();
        assert_eq!(all, 6);
        // One file: one part with the whole brief.
        let plan = heuristic_plan("x", "Only main.rs here", 4);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].brief, "Only main.rs here");
        // No file: still one part.
        assert_eq!(heuristic_plan("x", "Nothing named", 4).len(), 1);
        // A path escaping the folder is not a part.
        assert_eq!(
            heuristic_plan("x", "../other/a.rs and b.rs", 4)[0].target_files,
            vec!["b.rs"]
        );
    }

    fn fanout(tasks: Vec<FanoutTask>) -> Fanout {
        let waves = build_waves(&tasks);
        Fanout {
            id: "x-1".into(),
            subtasks: tasks,
            waves,
            ..Default::default()
        }
    }

    fn set(f: &mut Fanout, id: &str, state: TaskState) {
        f.task_mut(id).unwrap().state = state;
    }

    #[test]
    fn next_step_walks_the_barrier() {
        let mut f = fanout(vec![t("a", &[]), t("b", &[]), t("c", &["a", "b"])]);
        assert_eq!(next_step(&f), Step::StartWave(0));
        set(&mut f, "a", TaskState::Running);
        set(&mut f, "b", TaskState::Queued);
        assert_eq!(next_step(&f), Step::Wait);
        set(&mut f, "a", TaskState::Returned);
        assert_eq!(next_step(&f), Step::Wait, "b is still queued");
        set(&mut f, "b", TaskState::Returned);
        assert_eq!(next_step(&f), Step::StartWave(1));
        f.wave = 1;
        assert_eq!(next_step(&f), Step::StartWave(1), "c is pending");
        set(&mut f, "c", TaskState::Running);
        assert_eq!(next_step(&f), Step::Wait);
        set(&mut f, "c", TaskState::Returned);
        assert_eq!(next_step(&f), Step::Finish);
        // A failure ends the wave with a question.
        let mut g = fanout(vec![t("a", &[]), t("b", &[]), t("c", &["a"])]);
        set(&mut g, "a", TaskState::Failed);
        set(&mut g, "b", TaskState::Running);
        assert_eq!(next_step(&g), Step::Wait, "b still runs");
        set(&mut g, "b", TaskState::Returned);
        assert_eq!(next_step(&g), Step::AskFailure(vec!["a".into()]));
        // Continue: dependents are skipped, the next wave has nothing to start.
        assert_eq!(skip_dependents(&mut g), vec!["c"]);
        g.wave = 1;
        assert_eq!(next_step(&g), Step::Finish);
        // Retry: the failed part is pending again in its wave.
        set(&mut g, "a", TaskState::Pending);
        g.wave = 0;
        assert_eq!(next_step(&g), Step::StartWave(0));
        // No waves at all.
        assert_eq!(next_step(&Fanout::default()), Step::Finish);
    }

    #[test]
    fn skipping_is_transitive() {
        let mut f = fanout(vec![
            t("a", &[]),
            t("b", &["a"]),
            t("c", &["b"]),
            t("d", &[]),
        ]);
        set(&mut f, "a", TaskState::Failed);
        let skipped = skip_dependents(&mut f);
        assert_eq!(skipped, vec!["b", "c"]);
        assert_eq!(f.task("d").unwrap().state, TaskState::Pending);
        assert_eq!(
            f.task("c").unwrap().reason.as_deref(),
            Some("b did not return")
        );
    }

    #[test]
    fn aggregate_counts_cuts_and_lists_files() {
        let mut f = fanout(vec![
            with_files(t("a", &[]), &["x.rs"]),
            with_files(t("b", &[]), &["y.rs"]),
            t("c", &["b"]),
        ]);
        set(&mut f, "a", TaskState::Returned);
        set(&mut f, "b", TaskState::Failed);
        f.task_mut("b").unwrap().reason = Some("stopped without a result".into());
        skip_dependents(&mut f);
        let long = "z".repeat(500);
        let results = [PartResult {
            task_id: "a",
            key: "TWR-5.1",
            by: "Subtask · OpenCode",
            text: &long,
        }];
        let out = aggregate(&f, &results, 300);
        assert!(
            out.starts_with("Fan-out x-1: 1 of 3 parts returned, 1 failed, 1 skipped (2 waves)."),
            "{out}"
        );
        assert!(
            out.contains("## a · TWR-5.1 · Subtask · OpenCode · returned"),
            "{out}"
        );
        assert!(
            out.contains("## b · failed: stopped without a result"),
            "{out}"
        );
        assert!(out.contains("## c · skipped: b did not return"), "{out}");
        assert!(
            out.contains("Files owned by the parts: x.rs, y.rs"),
            "{out}"
        );
        assert!(out.contains("were not redone"), "{out}");
        assert!(
            out.matches('z').count() < 120,
            "each part gets its share of the cap: {out}"
        );
        set(&mut f, "b", TaskState::Returned);
        set(&mut f, "c", TaskState::Returned);
        assert!(aggregate(&f, &[], 300).contains("Review the combined changes before"));
    }

    #[test]
    fn parse_plan_takes_agent_args_and_planner_json() {
        let (tasks, analysis) = parse_plan(&json!({
            "analysis": "two files",
            "subtasks": [
                {"id": 1, "title": "A", "description": "do a", "target_file": "a.rs", "tier": "Low"},
                {"id": "s2", "brief": "do b\nmore", "depends_on": [1], "target_files": ["b.rs"], "read_only": true, "acceptance": ["builds"]}
            ]
        }))
        .unwrap();
        assert_eq!(analysis, "two files");
        assert_eq!(tasks[0].id, "s1");
        assert_eq!(tasks[0].brief, "do a");
        assert_eq!(tasks[0].target_files, vec!["a.rs"]);
        assert_eq!(tasks[0].tier.as_deref(), Some("low"));
        assert_eq!(tasks[1].depends_on, vec!["s1"]);
        assert_eq!(tasks[1].title, "do b", "title from the brief's first line");
        assert!(tasks[1].read_only);
        assert_eq!(tasks[1].acceptance, vec!["builds"]);
        assert!(validate(&tasks, &Limits::default()).is_ok());
        // "1" as an id and 1 as a dependency name the same part.
        let (mixed, _) = parse_plan(&json!([
            {"id": "1", "title": "a"},
            {"id": "2", "title": "b", "depends_on": [1]}
        ]))
        .unwrap();
        assert_eq!(mixed[0].id, "s1");
        assert_eq!(mixed[1].depends_on, vec!["s1"]);
        assert!(validate(&mixed, &Limits::default()).is_ok());
        let (bare, _) = parse_plan(&json!([{"title": "x"}])).unwrap();
        assert_eq!(bare[0].id, "s1");
        assert!(parse_plan(&json!({"nope": 1})).is_err());
        assert!(parse_plan(&json!("text")).is_err());
        assert!(parse_plan(&json!({"subtasks": [1]})).is_err());
    }
}
