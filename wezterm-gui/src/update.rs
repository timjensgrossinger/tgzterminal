use crate::ICON_DATA;
use anyhow::anyhow;
use config::{configuration, wezterm_version};
use http_req::request::{HttpVersion, Request};
use http_req::uri::Uri;
use mux::connui::ConnectionUI;
use serde::*;
use std::convert::TryFrom;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use termwiz::cell::{Hyperlink, Underline};
use termwiz::color::AnsiColor;
use termwiz::escape::csi::{Cursor, Sgr};
use termwiz::escape::osc::{ITermDimension, ITermFileData, ITermProprietary};
use termwiz::escape::{OneBased, OperatingSystemCommand, CSI};
use wezterm_toast_notification::*;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Release {
    pub url: String,
    pub body: String,
    pub html_url: String,
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Asset {
    pub name: String,
    pub size: usize,
    pub url: String,
    pub browser_download_url: String,
}

fn get_github_release_info(uri: &str) -> anyhow::Result<Release> {
    let uri = Uri::try_from(uri)?;

    let mut latest = Vec::new();
    let _res = Request::new(&uri)
        .version(HttpVersion::Http10)
        .header(
            "User-Agent",
            &format!("{}/{}", crate::brand::PRODUCT_NAME, wezterm_version()),
        )
        .send(&mut latest)
        .map_err(|e| anyhow!("failed to query github releases: {}", e))?;

    /*
    println!("Status: {} {}", _res.status_code(), _res.reason());
    println!("{}", String::from_utf8_lossy(&latest));
    */

    let latest: Release = serde_json::from_slice(&latest)?;
    Ok(latest)
}

pub fn get_latest_release_info() -> anyhow::Result<Release> {
    let uri = format!(
        "https://api.github.com/repos/{}/releases/latest",
        crate::brand::GITHUB_REPO
    );
    get_github_release_info(&uri)
}

#[allow(unused)]
pub fn get_nightly_release_info() -> anyhow::Result<Release> {
    let uri = format!(
        "https://api.github.com/repos/{}/releases/tags/nightly",
        crate::brand::GITHUB_REPO
    );
    get_github_release_info(&uri)
}

/// Returns true if `latest_tag` represents a newer release than `current_tag`.
///
/// Handles the fork's `tgz-vYYYY.MM.patch` scheme correctly:
/// strips `tgz-v` or `v` prefixes, splits on `.` and `-`, and compares
/// components numerically where possible. A tag in upstream format
/// (`20240203-…`) is never considered newer than a `tgz-v` build.
pub fn release_tag_is_newer(latest_tag: &str, current_tag: &str) -> bool {
    fn strip_prefix(tag: &str) -> &str {
        tag.strip_prefix("tgz-v")
            .or_else(|| tag.strip_prefix("v"))
            .unwrap_or(tag)
    }

    let latest_stripped = strip_prefix(latest_tag);
    let current_stripped = strip_prefix(current_tag);

    // If one is tgz-v and the other is not, they are incomparable formats.
    // Never report an upstream-format tag as an update for a tgz build.
    let latest_is_tgz = latest_tag.starts_with("tgz-");
    let current_is_tgz = current_tag.starts_with("tgz-");
    if current_is_tgz && !latest_is_tgz {
        return false;
    }

    let parse_parts = |s: &str| -> Vec<Option<u64>> {
        s.split(|c| c == '.' || c == '-')
            .map(|part| part.parse::<u64>().ok())
            .collect()
    };

    let latest_parts = parse_parts(latest_stripped);
    let current_parts = parse_parts(current_stripped);

    let len = latest_parts.len().max(current_parts.len());
    for i in 0..len {
        let l = latest_parts.get(i).copied().flatten();
        let c = current_parts.get(i).copied().flatten();
        match (l, c) {
            (Some(lv), Some(cv)) => {
                if lv != cv {
                    return lv > cv;
                }
            }
            _ => {
                // Fall back to lexicographic for non-numeric components
                let ls = latest_stripped;
                let cs = current_stripped;
                return ls > cs;
            }
        }
    }
    false
}

lazy_static::lazy_static! {
    static ref UPDATER_WINDOW: Mutex<Option<ConnectionUI>> = Mutex::new(None);
}

pub fn load_last_release_info_and_set_banner() {
    if !configuration().check_for_updates {
        return;
    }

    let update_file_name = config::DATA_DIR.join("check_update");
    if let Ok(data) = std::fs::read(update_file_name) {
        let latest: Release = match serde_json::from_slice(&data) {
            Ok(d) => d,
            Err(_) => return,
        };

        let current = wezterm_version();
        let force_ui = std::env::var_os("WEZTERM_ALWAYS_SHOW_UPDATE_UI").is_some();
        if !release_tag_is_newer(latest.tag_name.as_str(), current) && !force_ui {
            return;
        }

        set_banner_from_release_info(&latest);
    }
}

fn set_banner_from_release_info(latest: &Release) {
    let mux = crate::Mux::get();
    let url = latest.html_url.clone();

    let icon = ITermFileData {
        name: None,
        size: Some(ICON_DATA.len()),
        width: ITermDimension::Automatic,
        height: ITermDimension::Cells(2),
        preserve_aspect_ratio: true,
        inline: true,
        do_not_move_cursor: false,
        data: ICON_DATA.to_vec(),
    };
    let icon = OperatingSystemCommand::ITermProprietary(ITermProprietary::File(Box::new(icon)));
    let top_line_pos = CSI::Cursor(Cursor::CharacterAndLinePosition {
        line: OneBased::new(1),
        col: OneBased::new(6),
    });
    let second_line_pos = CSI::Cursor(Cursor::CharacterAndLinePosition {
        line: OneBased::new(2),
        col: OneBased::new(6),
    });
    let link_on = OperatingSystemCommand::SetHyperlink(Some(Hyperlink::new(url)));
    let underline_color = CSI::Sgr(Sgr::UnderlineColor(AnsiColor::Blue.into()));
    let underline_on = CSI::Sgr(Sgr::Underline(Underline::Single));
    let reset = CSI::Sgr(Sgr::Reset);
    let link_off = OperatingSystemCommand::SetHyperlink(None);
    mux.set_banner(Some(format!(
        "{}{}{} Update Available\r\n{}{}{}{}Click to see release details{}{}\r\n",
        icon,
        top_line_pos,
        crate::brand::PRODUCT_NAME,
        second_line_pos,
        link_on,
        underline_color,
        underline_on,
        link_off,
        reset,
    )));
}

fn schedule_set_banner_from_release_info(latest: &Release) {
    let current = wezterm_version();
    if !release_tag_is_newer(latest.tag_name.as_str(), current) {
        return;
    }
    promise::spawn::spawn_into_main_thread({
        let latest = latest.clone();
        async move {
            set_banner_from_release_info(&latest);
        }
    })
    .detach();
}

/// Returns true if the provided socket path is dead.
fn update_checker() {
    // Compute how long we should sleep for;
    // if we've never checked, give it a few seconds after the first
    // launch, otherwise compute the interval based on the time of
    // the last check.
    let update_interval = Duration::from_secs(configuration().check_for_updates_interval_seconds);
    let initial_interval = Duration::from_secs(10);

    let force_ui = std::env::var_os("WEZTERM_ALWAYS_SHOW_UPDATE_UI").is_some();

    let update_file_name = config::DATA_DIR.join("check_update");
    let delay = update_file_name
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map_err(|_| ())
        .and_then(|systime| {
            let elapsed = systime.elapsed().unwrap_or(Duration::new(0, 0));
            update_interval.checked_sub(elapsed).ok_or(())
        })
        .unwrap_or(initial_interval);

    std::thread::sleep(if force_ui { initial_interval } else { delay });

    let my_sock = config::RUNTIME_DIR.join(format!("gui-sock-{}", unsafe { libc::getpid() }));

    loop {
        // Figure out which other wezterm-guis are running.
        // We have a little "consensus protocol" to decide which
        // of us will show the toast notification or show the update
        // window: the one of us that sorts first in the list will
        // own doing that, so that if there are a dozen gui processes
        // running, we don't spam the user with a lot of notifications.
        let socks = wezterm_client::discovery::discover_gui_socks();

        if configuration().check_for_updates {
            if let Ok(latest) = get_latest_release_info() {
                schedule_set_banner_from_release_info(&latest);
                let current = wezterm_version();
                if release_tag_is_newer(latest.tag_name.as_str(), current) || force_ui {
                    log::info!(
                        "latest release {} is newer than current build {}",
                        latest.tag_name,
                        current
                    );

                    let url = latest.html_url.clone();

                    if force_ui || socks.is_empty() || socks[0] == my_sock {
                        persistent_toast_notification_with_click_to_open_url(
                            &format!("{} Update Available", crate::brand::PRODUCT_NAME),
                            "Click to see release details",
                            &url,
                        );
                    }
                }

                config::create_user_owned_dirs(update_file_name.parent().unwrap()).ok();

                // Record the time of this check
                if let Ok(f) = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&update_file_name)
                {
                    serde_json::to_writer_pretty(f, &latest).ok();
                }
            }
        }

        std::thread::sleep(Duration::from_secs(
            configuration().check_for_updates_interval_seconds,
        ));
    }
}

pub fn start_update_checker() {
    static CHECKER_STARTED: AtomicBool = AtomicBool::new(false);
    if let Ok(false) =
        CHECKER_STARTED.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
    {
        std::thread::Builder::new()
            .name("update_checker".into())
            .spawn(update_checker)
            .expect("failed to spawn update checker thread");
    }
}

#[cfg(test)]
mod tests {
    use super::release_tag_is_newer;

    #[test]
    fn tgz_patch_ordering() {
        assert!(release_tag_is_newer("tgz-v2026.07.10", "tgz-v2026.07.2"));
        assert!(!release_tag_is_newer("tgz-v2026.07.2", "tgz-v2026.07.10"));
    }

    #[test]
    fn tgz_month_ordering() {
        assert!(release_tag_is_newer("tgz-v2026.08.1", "tgz-v2026.07.10"));
        assert!(!release_tag_is_newer("tgz-v2026.07.10", "tgz-v2026.08.1"));
    }

    #[test]
    fn equal_is_not_newer() {
        assert!(!release_tag_is_newer("tgz-v2026.07.2", "tgz-v2026.07.2"));
    }

    #[test]
    fn upstream_tag_not_newer_than_tgz() {
        assert!(!release_tag_is_newer(
            "20240203-110809-5046fc22",
            "tgz-v2026.07.1"
        ));
    }
}
