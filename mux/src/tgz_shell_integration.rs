//! zsh shell-integration injection (TGZTerminal, `assist.shell_integration`).
//!
//! Command blocks and exit codes need OSC 133 marks, and a stock macOS zsh
//! emits none unless the user sources `wezterm.sh` themselves. This injects
//! it the way Ghostty does: the shell starts with `ZDOTDIR` pointing at a
//! directory of ours whose `.zshenv` puts the user's `ZDOTDIR` back, sources
//! the user's own `.zshenv`, and (interactive shells only) arranges for
//! `wezterm.sh` to load at the first prompt. zsh reads `.zprofile`,
//! `.zshrc` and `.zlogin` from the restored `ZDOTDIR`, so the user's startup
//! files run exactly as before.
//!
//! `wezterm.sh` wraps `PS1` from a `precmd` hook. Prompt frameworks (p10k,
//! oh-my-zsh themes, starship) rebuild `PS1` from their own `precmd` hook,
//! and whichever runs last wins, so the loader keeps its hook at the end of
//! `precmd_functions` instead of letting `wezterm.sh` register its own.
//! powerlevel10k re-asserts its own hook as the last one on every prompt, so
//! for it the loader switches on p10k's built-in OSC 133 marks through
//! `__p9k_force_term_shell_integration`, as kitty and VS Code do.
//!
//! Applied only to the local domain's default program, only when that is
//! zsh, and only on unix. Anything else is left alone.

use portable_pty::CommandBuilder;
use std::path::{Path, PathBuf};

const WEZTERM_SH: &str = include_str!("../../assets/shell-integration/wezterm.sh");

const ZSHENV: &str = r#"# TGZTerminal shell integration loader, injected through ZDOTDIR.
# Puts your ZDOTDIR back and sources your own .zshenv; see
# mux/src/tgz_shell_integration.rs. Turn it off with
# config.assist.shell_integration = false.
if [[ -n "${TGZ_ZSH_ZDOTDIR+X}" ]]; then
  builtin export ZDOTDIR="$TGZ_ZSH_ZDOTDIR"
  builtin unset TGZ_ZSH_ZDOTDIR
else
  builtin unset ZDOTDIR
fi

# powerlevel10k rebuilds PS1 from its own precmd, after anyone else's
# wrapping, but marks its own prompt (133;A/B) when forced to: the flag
# kitty's and VS Code's shell integration set (POWERLEVEL9K_* settings would
# be unset again by ~/.p10k.zsh). Without p10k it is an unused variable.
(( ${+__p9k_force_term_shell_integration} )) || builtin typeset -gri __p9k_force_term_shell_integration=1

{
  builtin typeset _tgz_file="${ZDOTDIR-$HOME}/.zshenv"
  [[ ! -r "$_tgz_file" ]] || builtin source -- "$_tgz_file"
} always {
  if [[ -o interactive && -n "${TGZ_SHELL_INTEGRATION-}" ]]; then
    builtin typeset -g _tgz_integration="$TGZ_SHELL_INTEGRATION"
    _tgz_set_status() { return $1 }
    _tgz_precmd() {
      builtin local ret=$?
      # Stay last, so a prompt framework's PS1 rebuild runs before the
      # marks are wrapped around it.
      if [[ "${precmd_functions[-1]}" != _tgz_precmd ]]; then
        precmd_functions=(${precmd_functions:#_tgz_precmd} _tgz_precmd)
      fi
      (( $+functions[__wezterm_semantic_precmd] )) || return 0
      _tgz_set_status $ret
      __wezterm_semantic_precmd
    }
    _tgz_deferred_init() {
      builtin local ret=$?
      precmd_functions=(${precmd_functions:#_tgz_deferred_init})
      # Load the functions; the loader registers the semantic hooks itself.
      builtin typeset -g WEZTERM_SHELL_SKIP_SEMANTIC_ZONES=1
      builtin source -- "$_tgz_integration"
      builtin unset WEZTERM_SHELL_SKIP_SEMANTIC_ZONES _tgz_integration
      # WEZTERM_SHELL_SKIP_ALL (or a broken file) defines nothing: stay out.
      (( $+functions[__wezterm_semantic_precmd] )) || return 0
      precmd_functions+=(_tgz_precmd)
      preexec_functions+=(__wezterm_semantic_preexec)
      _tgz_set_status $ret
      _tgz_precmd
    }
    precmd_functions=(_tgz_deferred_init $precmd_functions)
  fi
  builtin unset _tgz_file TGZ_SHELL_INTEGRATION
}
"#;

/// Where the loader lives: `<data dir>/shell-integration/zsh`.
fn integration_dir() -> PathBuf {
    config::DATA_DIR.join("shell-integration").join("zsh")
}

/// Write `contents` to `path` unless it already holds exactly that.
fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Is this command the default program, and is that program zsh?
pub fn wants_injection(cmd: &CommandBuilder) -> bool {
    if !cmd.is_default_prog() {
        return false;
    }
    #[cfg(unix)]
    {
        let shell = cmd.get_shell();
        Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.trim_start_matches('-') == "zsh")
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Point a default-program zsh at the loader. Best effort: a failure to
/// write the files is logged and the shell starts without integration.
pub fn inject(cmd: &mut CommandBuilder) {
    if !wants_injection(cmd) {
        return;
    }
    let dir = integration_dir();
    let script = dir.join("wezterm.sh");
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| write_if_changed(&dir.join(".zshenv"), ZSHENV))
        .and_then(|()| write_if_changed(&script, WEZTERM_SH));
    if let Err(err) = written {
        log::warn!(
            "shell integration: could not write {}: {err:#}",
            dir.display()
        );
        return;
    }
    if let Some(orig) = cmd.get_env("ZDOTDIR").map(|v| v.to_os_string()) {
        cmd.env("TGZ_ZSH_ZDOTDIR", orig);
    }
    cmd.env("ZDOTDIR", dir.as_os_str());
    cmd.env("TGZ_SHELL_INTEGRATION", script.as_os_str());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_program_is_never_injected() {
        let cmd = CommandBuilder::new("zsh");
        assert!(!wants_injection(&cmd));
    }

    #[test]
    fn the_loader_restores_zdotdir_and_keeps_its_hook_last() {
        assert!(ZSHENV.contains(r#"builtin export ZDOTDIR="$TGZ_ZSH_ZDOTDIR""#));
        assert!(ZSHENV.contains("builtin unset ZDOTDIR"));
        assert!(ZSHENV.contains("precmd_functions=(${precmd_functions:#_tgz_precmd} _tgz_precmd)"));
        // wezterm.sh must not register its own semantic hooks; the loader does.
        assert!(ZSHENV.contains("WEZTERM_SHELL_SKIP_SEMANTIC_ZONES=1"));
        // p10k marks its own prompt (the flag kitty and VS Code set too).
        assert!(ZSHENV.contains("typeset -gri __p9k_force_term_shell_integration=1"));
        assert!(WEZTERM_SH.contains("__wezterm_semantic_precmd()"));
    }
}
