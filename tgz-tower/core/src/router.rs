//! Task routing: how hard is this task, and how much thinking does it need?
//!
//! A Rust port of Threnody's heuristic router (`shared/router.py`): keyword
//! tiers with capped hits, hard overrides, speed/quality intent, reasoning
//! signals, and a security risk floor over the task text. The result is a
//! tier (`low` / `medium` / `high`), an effort, and a reason that lists every
//! signal that fired, so a routing decision can always be explained.
//!
//! Threnody's project/time modifiers and file-content evidence are left out:
//! Tower routes from the flight text alone. Triage may raise the tier, never
//! lower it below the risk floor.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Low,
    Medium,
    High,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Low, Tier::Medium, Tier::High];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Low => "low",
            Tier::Medium => "medium",
            Tier::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "small" | "simple" => Some(Tier::Low),
            "medium" | "mid" => Some(Tier::Medium),
            "high" | "large" | "complex" => Some(Tier::High),
            _ => None,
        }
    }

    pub fn up(self) -> Option<Tier> {
        match self {
            Tier::Low => Some(Tier::Medium),
            Tier::Medium => Some(Tier::High),
            Tier::High => None,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Duration {
    Short,
    Medium,
    Long,
}

/// How much the agent should think: Threnody's `reasoning_effort`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub tier: Tier,
    pub score: f64,
    /// A keyword forced the tier (`architect`, `typo`, …).
    pub overridden: bool,
    /// The lowest tier this task may run at (risk vocabulary, reasoning).
    pub floor: Tier,
    pub duration: Duration,
    pub effort: Effort,
    pub files: usize,
    /// Every signal that fired, e.g. `refactor(+0.20), multi_file(+0.10)`.
    pub reason: String,
}

const BASE_SCORE: f64 = 0.10;
const LOW_MAX: f64 = 0.20;
const MEDIUM_MAX: f64 = 0.60;
const LOW_OVERRIDE_DELTA: f64 = -0.20;
const SYSTEMS_LANGUAGE_BONUS: f64 = 0.15;
const DURATION_LONG_FILES: usize = 4;

/// (level weight, hit cap) for high / medium / low signals.
const HIGH_W: (f64, usize) = (0.20, 2);
const MEDIUM_W: (f64, usize) = (0.12, 2);

/// Substring-matched complexity signals (Threnody's DEFAULT_COMPLEXITY_SIGNALS).
const HIGH_SIGNALS: &[&str] = &[
    "refactor",
    "redesign",
    "optimize",
    "performance",
    "concurren",
    "parallel",
    "async",
    "distributed",
    "connection pool",
    "database layer",
    "microservice",
    "end-to-end",
    "e2e",
    "full stack",
    "cross-cutting",
    "system-wide",
    "multi-step",
    "pipeline",
    "workflow engine",
    "lexer",
    "grammar",
    "recursive descent",
    "abstract syntax",
    "codegen",
    "bytecode",
    "type inference",
    "neural network",
    "training loop",
    "inference engine",
    "backprop",
    "tensor",
    "shader",
    "gpu",
    "vulkan",
    "opengl",
    "wgpu",
    "render pipeline",
    "rasteriz",
    "firmware",
    "embedded",
    "interrupt handler",
    "bare metal",
    "bootloader",
    "tokio",
    "lifetimes",
    "borrow checker",
    "unsafe rust",
    "bindgen",
    "lock-free",
    "zero copy",
    "memory pool",
    "memory layout",
    "ratatui",
    "egui",
    "tauri",
];

const MEDIUM_SIGNALS: &[&str] = &[
    "implement",
    "integrate",
    "migrate",
    "test suite",
    "error handling",
    "middleware",
    "authentication",
    "authorization",
    "build",
    "set up",
    "configure",
    "scaffold",
    "bootstrap",
    "wire up",
    "connect",
    "hook up",
    "register",
    "provision",
    "sqlite",
    "sqlx",
    "diesel",
    "migration",
    "fastapi",
    "django",
    "axum",
    "kafka",
    "rabbitmq",
    "pub/sub",
    "protobuf",
    "grpc",
    "avro",
    "websocket",
    "integration",
    "rewrite",
    "overhaul",
    "harden",
    "invert",
];

/// Short tokens that need whole-word matching ("gui" is inside "guide").
const HIGH_WORD_SIGNALS: &[&str] = &["gui", "tui", "ffi"];
const MEDIUM_WORD_SIGNALS: &[&str] = &["rails", "spring", "crate", "redis"];

const SYSTEMS_LANGUAGES: &[&str] = &[
    "rust", "golang", "c++", "cpp", "zig", "nim", "haskell", "ocaml",
];

const LOW_OVERRIDES: &[&str] = &[
    "docstring",
    "type hint",
    "add comment",
    "rename",
    "lint",
    "simple",
    "one-liner",
    "list files",
    "find files",
    "boilerplate",
    "typo",
    "spelling",
    "bump version",
    "update version",
    "changelog",
    "add log",
    "add logging statement",
    "print statement",
];

const HIGH_OVERRIDES: &[&str] = &[
    "architect",
    "architecture",
    "design",
    "deep security review",
    "security-critical",
    "critical security",
    "threat model",
    "multi-tenant",
    "system design",
    "compliance",
    "penetration test",
    "vulnerability",
    "cryptograph",
    "zero trust",
    "audit trail",
    "rbac",
    "role-based",
    "sso",
    "oauth",
    "saml",
    "pen test",
    "pentest",
    "scaffold",
    "database connection",
    "systems programming",
    "memory safety",
    "kernel module",
];

const SPEED_SIGNALS: &[(&str, f64)] = &[
    ("quick", -0.15),
    ("just", -0.12),
    ("fast", -0.15),
    ("rough", -0.12),
    ("simple", -0.10),
    ("wip", -0.12),
    ("draft", -0.10),
    ("trivial", -0.12),
    ("tiny", -0.10),
    ("minor", -0.10),
];

const QUALITY_SIGNALS: &[(&str, f64)] = &[
    ("thorough", 0.15),
    ("double check", 0.12),
    ("production", 0.15),
    ("careful", 0.12),
    ("comprehensive", 0.15),
    ("review", 0.10),
    ("make sure", 0.12),
    ("robust", 0.12),
    ("bulletproof", 0.15),
    ("enterprise", 0.12),
];

const REASONING_SIGNALS: &[(&str, f64)] = &[
    ("brainstorm", 0.18),
    ("ideate", 0.18),
    ("compose", 0.14),
    ("draft", 0.10),
    ("suggest", 0.10),
    ("generate ideas", 0.18),
    ("tradeoff", 0.18),
    ("pros and cons", 0.18),
    ("compare", 0.14),
    ("recommend", 0.14),
    ("evaluate", 0.18),
    ("critique", 0.18),
    ("prioritize", 0.14),
    ("weigh", 0.14),
    ("explain why", 0.18),
    ("think through", 0.18),
    ("justify", 0.14),
    ("reasoning", 0.18),
    ("analyze", 0.14),
    ("tone", 0.14),
    ("voice", 0.14),
    ("compelling", 0.18),
    ("persuasive", 0.14),
    ("engaging", 0.12),
    ("narrative", 0.14),
];

/// Threnody's PROSE_RISK_TOKENS: risk vocabulary that, in a task sentence,
/// implies security-sensitive change work. Matched at a word start, so
/// `auth` also catches `authentication` and `credential` `credentials`.
pub const PROSE_RISK_TOKENS: &[&str] = &[
    "auth",
    "oauth",
    "saml",
    "jwt",
    "crypto",
    "cryptograph",
    "encrypt",
    "decrypt",
    "password",
    "secret",
    "credential",
    "keychain",
    "token",
    "api_key",
    "injection",
    "sanitiz",
    "redact",
    "untrusted",
    "privacy",
    "pii",
    "xss",
    "csrf",
    "blocklist",
    "allowlist",
];

/// Source and config extensions that count as a work file.
const WORK_EXTS: &[&str] = &[
    "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "html", "htm", "css", "scss", "vue",
    "svelte", "go", "rs", "java", "kt", "rb", "cs", "lua", "c", "h", "cpp", "hpp", "cc", "sh",
    "bash", "zsh", "swift", "ex", "exs", "tf", "sql", "proto", "yaml", "yml", "json", "toml",
    "ini", "cfg",
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Regex `\b` at byte offset `i` of `s`.
fn boundary(s: &str, i: usize) -> bool {
    let before = s[..i].chars().next_back().map(is_word).unwrap_or(false);
    let after = s[i..].chars().next().map(is_word).unwrap_or(false);
    before != after
}

/// How often `kw` occurs as `\bkw\b` (non-overlapping, like `re.findall`).
pub fn word_hits(text: &str, kw: &str) -> usize {
    let mut n = 0;
    let mut from = 0;
    while let Some(pos) = text[from..].find(kw) {
        let start = from + pos;
        let end = start + kw.len();
        if boundary(text, start) && boundary(text, end) {
            n += 1;
            from = end;
        } else {
            from = start
                + text[start..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(1);
        }
        if from >= text.len() {
            break;
        }
    }
    n
}

pub fn has_word(text: &str, kw: &str) -> bool {
    word_hits(text, kw) > 0
}

/// `\bkw` (a word that starts with `kw`).
fn has_word_start(text: &str, kw: &str) -> bool {
    text.match_indices(kw).any(|(i, _)| boundary(text, i))
}

/// Does the task text carry security risk vocabulary?
pub fn has_prose_risk(task_lower: &str) -> bool {
    PROSE_RISK_TOKENS
        .iter()
        .any(|t| has_word_start(task_lower, t))
}

/// Matches of `\b\w+\.\w{1,4}\b`, Threnody's loose "file reference" count.
fn file_refs(text: &str) -> usize {
    let mut n = 0;
    for run in text.split(|c: char| !(is_word(c) || c == '.')) {
        let segs: Vec<&str> = run.split('.').collect();
        let mut i = 0;
        while i + 1 < segs.len() {
            let ext = segs[i + 1].chars().count();
            if !segs[i].is_empty() && (1..=4).contains(&ext) {
                n += 1;
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    n
}

/// Distinct work files named in a task (`src/auth.rs`, `cli.py`).
pub fn count_task_files(task: &str) -> usize {
    let lower = task.to_lowercase();
    let mut seen = std::collections::BTreeSet::new();
    for run in lower.split(|c: char| !(is_word(c) || matches!(c, '.' | '/' | '-'))) {
        let tok = run.trim_end_matches(|c: char| !is_word(c));
        if tok.ends_with('/') {
            continue;
        }
        if let Some((stem, ext)) = tok.rsplit_once('.') {
            let name = stem.rsplit('/').next().unwrap_or(stem);
            if !name.is_empty() && WORK_EXTS.contains(&ext) {
                seen.insert(tok.to_string());
            }
        }
    }
    seen.len()
}

fn sys_file_refs(text: &str) -> usize {
    let mut n = 0;
    for run in text.split(|c: char| !(is_word(c) || c == '.')) {
        let segs: Vec<&str> = run.split('.').collect();
        for w in segs.windows(2) {
            if !w[0].is_empty() && matches!(w[1], "rs" | "go" | "cpp" | "cc" | "cxx" | "c") {
                n += 1;
            }
        }
    }
    n
}

fn tier_from_score(score: f64) -> Tier {
    if score <= LOW_MAX {
        Tier::Low
    } else if score <= MEDIUM_MAX {
        Tier::Medium
    } else {
        Tier::High
    }
}

fn duration_for(score: f64, files: usize) -> Duration {
    if files >= DURATION_LONG_FILES || score > MEDIUM_MAX {
        Duration::Long
    } else if score <= LOW_MAX && files <= 1 {
        Duration::Short
    } else {
        Duration::Medium
    }
}

pub fn effort_for(duration: Duration, tier: Tier) -> Effort {
    if duration == Duration::Short || tier == Tier::Low {
        Effort::Low
    } else if duration == Duration::Long || tier == Tier::High {
        Effort::High
    } else {
        Effort::Medium
    }
}

fn high_signal(task: &str) -> bool {
    HIGH_SIGNALS.iter().any(|k| task.contains(k))
        || HIGH_WORD_SIGNALS.iter().any(|k| has_word(task, k))
}

fn raw_score(task: &str, matched: &mut Vec<String>) -> f64 {
    let mut score = BASE_SCORE;
    let mut credit = |hits: &mut usize, (w, cap): (f64, usize), kw: &str, m: &mut Vec<String>| {
        if *hits >= cap {
            m.push(format!("{kw}(capped)"));
        } else {
            *hits += 1;
            score += w;
            m.push(format!("{kw}(+{w:.2})"));
        }
    };
    let (mut high, mut medium) = (0, 0);
    for kw in HIGH_SIGNALS.iter().filter(|k| task.contains(*k)) {
        credit(&mut high, HIGH_W, kw, matched);
    }
    for kw in MEDIUM_SIGNALS.iter().filter(|k| task.contains(*k)) {
        credit(&mut medium, MEDIUM_W, kw, matched);
    }
    for kw in HIGH_WORD_SIGNALS.iter().filter(|k| has_word(task, k)) {
        credit(&mut high, HIGH_W, kw, matched);
    }
    for kw in MEDIUM_WORD_SIGNALS.iter().filter(|k| has_word(task, k)) {
        credit(&mut medium, MEDIUM_W, kw, matched);
    }
    let words = task.split_whitespace().count();
    if words > 30 {
        score += 0.10;
        matched.push("long_prompt(+0.10)".into());
    } else if words > 15 {
        score += 0.05;
        matched.push("medium_prompt(+0.05)".into());
    }
    if file_refs(task) >= 3 {
        score += 0.10;
        matched.push("multi_file(+0.10)".into());
    }
    if let Some(lang) = SYSTEMS_LANGUAGES.iter().find(|l| has_word(task, l)) {
        score += SYSTEMS_LANGUAGE_BONUS;
        matched.push(format!("systems_lang:{lang}(+0.15)"));
    }
    let sys = sys_file_refs(task);
    if sys >= 1 {
        score += 0.15;
        matched.push(format!("sys_lang_files:{sys}(+0.15)"));
    }
    score.min(1.0)
}

fn weighted(task: &str, table: &[(&str, f64)], label: &str, matched: &mut Vec<String>) -> f64 {
    let mut sum = 0.0;
    for (kw, w) in table {
        if has_word(task, kw) {
            sum += w;
            matched.push(format!("{label}{kw}({w:+.2})"));
        }
    }
    sum
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Classify a task by its text.
pub fn classify(task: &str) -> Route {
    let task = task.to_lowercase();
    let task = task.trim();
    let files = count_task_files(task);
    let risky = has_prose_risk(task);
    let risk_floor = if risky { Tier::Medium } else { Tier::Low };

    if let Some(kw) = HIGH_OVERRIDES.iter().find(|k| has_word(task, k)) {
        let duration = if files >= DURATION_LONG_FILES {
            Duration::Long
        } else {
            Duration::Medium
        };
        return Route {
            tier: Tier::High,
            score: 0.90,
            overridden: true,
            floor: risk_floor,
            duration,
            effort: effort_for(duration, Tier::High),
            files,
            reason: format!("keyword override → high: '{kw}'"),
        };
    }

    let mut matched = Vec::new();
    let raw = raw_score(task, &mut matched);

    // The low override only nudges a task that is plainly small.
    let mut low_delta = 0.0;
    if raw < LOW_MAX && !risky && file_refs(task) < 3 && !high_signal(task) {
        if let Some(kw) = LOW_OVERRIDES.iter().find(|k| has_word(task, k)) {
            low_delta = LOW_OVERRIDE_DELTA;
            matched.push(format!("low_override:'{kw}'({LOW_OVERRIDE_DELTA:+.2})"));
        }
    }
    let intent = weighted(task, SPEED_SIGNALS, "speed:", &mut matched)
        + weighted(task, QUALITY_SIGNALS, "quality:", &mut matched);
    let effective = round2((raw + low_delta + intent).clamp(0.0, 1.0));

    let mut reasoning_m = Vec::new();
    let reasoning = weighted(task, REASONING_SIGNALS, "reasoning:", &mut reasoning_m).min(1.0);
    let reasoning_fired = reasoning > effective && reasoning > 0.15;
    let score = if reasoning_fired {
        matched.extend(reasoning_m);
        round2(reasoning)
    } else {
        effective
    };

    let mut tier = tier_from_score(score);
    let mut floor = risk_floor;
    if reasoning_fired {
        floor = floor.max(Tier::Medium);
    }
    let mut mods = Vec::new();
    if low_delta != 0.0 {
        mods.push(format!("low_override={low_delta:+.2}"));
    }
    if intent != 0.0 {
        mods.push(format!("intent={intent:+.2}"));
    }
    if reasoning_fired {
        mods.push(format!("reasoning={reasoning:+.2}"));
    }
    if tier < floor {
        tier = floor;
        mods.push(if risky {
            "security_floor=medium".to_string()
        } else {
            "reasoning_floor=medium".to_string()
        });
    }
    let signals = if matched.is_empty() {
        "base score only".to_string()
    } else {
        matched.join(", ")
    };
    let reason = if mods.is_empty() {
        format!("score={score:.2} [{signals}] → {tier}")
    } else {
        format!(
            "raw={raw:.2}, {}, effective={score:.2} [{signals}] → {tier}",
            mods.join(", ")
        )
    };
    let duration = duration_for(score, files);
    Route {
        tier,
        score,
        overridden: false,
        floor,
        duration,
        effort: effort_for(duration, tier),
        files,
        reason,
    }
}

/// Combine the heuristic route with a triage opinion: triage may move the
/// tier either way, but never below the heuristic's floor.
pub fn refine(route: &Route, triage: Option<Tier>) -> Tier {
    match triage {
        Some(t) => t.max(route.floor),
        None => route.tier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors Threnody's tests/test_router.py.

    #[test]
    fn plain_text_is_low() {
        assert_eq!(classify("hello world").tier, Tier::Low);
        let r = classify("add a docstring to this function");
        assert_eq!(r.tier, Tier::Low);
        assert_eq!(r.effort, Effort::Low);
    }

    #[test]
    fn low_override_is_suppressed_by_risk() {
        let r = classify("add a docstring and refactor credential keychain handling in auth.py");
        assert!(r.tier >= Tier::Medium, "{}", r.reason);
        assert!(!r.overridden);
    }

    #[test]
    fn low_override_is_suppressed_by_a_high_signal() {
        let r = classify(
            "Rename this helper inside the async, distributed, concurrency-sensitive permission module.",
        );
        assert!(!r.reason.contains("low_override"), "{}", r.reason);
        assert!(r.tier >= Tier::Medium);
    }

    #[test]
    fn low_override_still_applies() {
        let r = classify("rename this variable");
        assert!(r.reason.contains("low_override"), "{}", r.reason);
        assert_eq!(r.tier, Tier::Low);
    }

    #[test]
    fn high_overrides() {
        let r = classify("do a deep security review of this module");
        assert_eq!((r.tier, r.overridden), (Tier::High, true));
        let r = classify("do a security review of this module");
        assert!(r.tier <= Tier::Medium && !r.overridden);
        let r = classify("Architect an OAuth authentication migration");
        assert_eq!((r.tier, r.overridden), (Tier::High, true));
    }

    #[test]
    fn routine_auth_work_is_medium_by_its_floor() {
        let r = classify("Implement authentication middleware across auth.py service.py cli.py");
        assert_eq!(r.tier, Tier::Medium, "{}", r.reason);
        assert_eq!(r.floor, Tier::Medium);
        assert!(!r.overridden);
    }

    #[test]
    fn intent_moves_the_score() {
        let normal = classify("implement message filtering logic");
        let quick = classify("quick implement message filtering logic");
        assert!(quick.score < normal.score);
        let normal = classify("add a helper function");
        let thorough = classify("thorough add a helper function");
        assert!(thorough.score > normal.score);
    }

    #[test]
    fn size_signals() {
        assert!(classify("update foo.py bar.js baz.ts to use new API")
            .reason
            .contains("multi_file"));
        let long = vec!["word"; 35].join(" ");
        assert!(classify(&long).reason.contains("long_prompt"));
    }

    #[test]
    fn complex_systems_work_is_high_with_high_effort() {
        let r =
            classify("refactor the tokio connection pool in src/pool.rs for lock-free performance");
        assert_eq!(r.tier, Tier::High, "{}", r.reason);
        assert_eq!(r.effort, Effort::High);
        // Hits per level are capped: vocabulary density is not difficulty.
        assert!(r.reason.contains("(capped)"));
    }

    #[test]
    fn reasoning_tasks_never_go_low() {
        let r = classify("brainstorm and compare a few names, weigh the tradeoff");
        assert!(r.tier >= Tier::Medium, "{}", r.reason);
        assert_eq!(r.floor, Tier::Medium);
    }

    #[test]
    fn word_matching_is_regex_like() {
        assert!(has_word("a gui app", "gui"));
        assert!(!has_word("read the guide", "gui"));
        assert_eq!(word_hits("design, design", "design"), 2);
        // `\bc\+\+\b` needs a word char after the `+`, as in Python.
        assert!(!has_word("in c++ code", "c++"));
        assert!(has_word_start("fix authentication", "auth"));
        assert!(!has_word_start("the author", "thor"));
        assert!(has_prose_risk("rotate the api tokens"));
        assert!(!has_prose_risk("make the sidebar wider"));
    }

    #[test]
    fn files_are_counted_once_and_only_work_files() {
        assert_eq!(
            count_task_files("edit src/a.rs and a.rs, then src/a.rs."),
            2
        );
        assert_eq!(count_task_files("update the README.md, e.g. the intro"), 0);
        assert_eq!(count_task_files("a.py b.py c.toml d.json"), 4);
        assert_eq!(
            classify("touch a.py b.py c.toml d.json").duration,
            Duration::Long
        );
    }

    #[test]
    fn triage_cannot_go_below_the_floor() {
        let r = classify("rotate the stored password hashes");
        assert_eq!(r.floor, Tier::Medium);
        assert_eq!(refine(&r, Some(Tier::Low)), Tier::Medium);
        assert_eq!(refine(&r, Some(Tier::High)), Tier::High);
        assert_eq!(refine(&classify("fix a typo"), Some(Tier::Low)), Tier::Low);
        assert_eq!(refine(&r, None), r.tier);
    }
}
