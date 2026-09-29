fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // If a file named `.tag` is present, we'll take its contents for the
    // version number that we report in wezterm -h.
    let mut ci_tag = String::new();
    if let Ok(tag) = std::fs::read("../.tag") {
        if let Ok(s) = String::from_utf8(tag) {
            ci_tag = s.trim().to_string();
            println!("cargo:rerun-if-changed=../.tag");
        }
    } else {
        // Otherwise we'll derive it from the git information

        if let Ok(repo) = git2::Repository::discover(".") {
            // Re-run whenever HEAD moves. Watching only the branch's loose
            // ref file was not enough: while refs are packed there is no
            // such file, a build made then watched nothing but build.rs and
            // reported that commit's version forever, and a checkout of
            // another branch was never noticed. The reflog changes on every
            // commit, checkout, reset and pull; HEAD, packed-refs and the
            // loose ref cover a disabled reflog. Refs live in the common
            // dir, which is not repo.path() in a linked worktree.
            let mut watch = vec![
                repo.path().join("logs").join("HEAD"),
                repo.path().join("HEAD"),
                repo.commondir().join("packed-refs"),
            ];
            if let Ok(ref_head) = repo.find_reference("HEAD") {
                if let Ok(resolved) = ref_head.resolve() {
                    if let Some(name) = resolved.name() {
                        watch.push(repo.commondir().join(name));
                    }
                }
            }
            for path in watch {
                if path.exists() {
                    let path = path.canonicalize().unwrap_or(path);
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }

            if let Ok(output) = std::process::Command::new("git")
                .args(&[
                    "-c",
                    "core.abbrev=8",
                    "show",
                    "-s",
                    "--format=%cd-%h",
                    "--date=format:%Y%m%d-%H%M%S",
                ])
                .output()
            {
                let info = String::from_utf8_lossy(&output.stdout);
                ci_tag = info.trim().to_string();
            }
        }
    }

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=WEZTERM_TARGET_TRIPLE={}", target);
    println!("cargo:rustc-env=WEZTERM_CI_TAG={}", ci_tag);
}
