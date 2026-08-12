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

/// The platforms the fork publishes release artifacts for.
///
/// Kept as an explicit parameter rather than a `cfg!` inside the selection
/// logic so that the asset matching for every platform is testable from any
/// host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    MacOS,
    Windows,
    Other,
}

impl Platform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOS
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// The release artifact a user on `platform` should download.
///
/// Asset names are produced by the release workflows and are stable:
/// `<Product>.dmg` on macOS, and on Windows `<Product>-Setup-<tag>.exe` next to
/// the portable `<Product>-windows-portable-<tag>.zip`. Both Windows artifacts
/// are always published, so `portable` decides which one this build should be
/// pointed at: replacing a portable extraction with an installer (or the reverse)
/// is not what the user asked for. `.sha256` sidecars never match, since the
/// extension check excludes them. Returns `None` on platforms we publish nothing
/// for, in which case callers fall back to the release page.
fn pick_asset_for(release: &Release, platform: Platform, portable: bool) -> Option<&Asset> {
    let product = crate::brand::PRODUCT_NAME;
    match platform {
        Platform::MacOS => {
            let dmg = format!("{}.dmg", product);
            release.assets.iter().find(|asset| asset.name == dmg)
        }
        Platform::Windows => {
            let setup = format!("{}-Setup", product);
            let portable_prefix = format!("{}-windows-portable", product);
            let find_setup = || {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name.starts_with(&setup) && asset.name.ends_with(".exe"))
            };
            let find_zip = || {
                release.assets.iter().find(|asset| {
                    asset.name.starts_with(&portable_prefix) && asset.name.ends_with(".zip")
                })
            };
            // Either way, fall back to the other kind rather than sending the
            // user to a release page with no download.
            if portable {
                find_zip().or_else(find_setup)
            } else {
                find_setup().or_else(find_zip)
            }
        }
        Platform::Other => None,
    }
}

fn pick_asset(release: &Release) -> Option<&Asset> {
    pick_asset_for(release, Platform::current(), config::portable_mode())
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

/// Persist the release metadata; the file's mtime doubles as the timestamp of
/// the last successful check.
fn cache_release(latest: &Release) {
    let update_file_name = config::DATA_DIR.join("check_update");
    config::create_user_owned_dirs(update_file_name.parent().unwrap()).ok();

    if let Ok(f) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&update_file_name)
    {
        serde_json::to_writer_pretty(f, latest).ok();
    }
}

/// Show the "update available" toast. Clicking it downloads this platform's
/// artifact directly; when the release has no artifact for us (Linux, or a
/// release whose upload failed) it falls back to the release page.
fn notify_update_available(latest: &Release) {
    let asset = pick_asset(latest);
    let url = asset
        .map(|asset| asset.browser_download_url.as_str())
        .unwrap_or(latest.html_url.as_str());
    let message = match asset {
        Some(asset) => format!("Click to download {}", asset.name),
        None => "Click to see release details".to_string(),
    };

    persistent_toast_notification_with_click_to_open_url(
        &format!(
            "{} {} is available",
            crate::brand::PRODUCT_NAME,
            latest.tag_name
        ),
        &message,
        url,
    );
}

/// A build made from a release tag reports `tgz-vYYYY.MM.PATCH`; a local or CI
/// build reports the git-derived `YYYYMMDD-HHMMSS-hash` form, which
/// `release_tag_is_newer` cannot meaningfully compare against a release tag.
fn is_release_build(current: &str) -> bool {
    current.starts_with("tgz-")
}

fn toast(title: &str, message: &str) {
    ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: None,
        timeout: Some(Duration::from_secs(10)),
    }
    .show();
}

/// One-shot update check, for the `CheckForUpdates` key assignment / command
/// palette entry. Always answers the user, even when already up to date, and
/// ignores both the `check_for_updates` setting and the multi-process
/// consensus used by the background checker: this was explicitly asked for.
pub fn check_for_updates_now() {
    if let Err(err) = std::thread::Builder::new()
        .name("update_check_now".into())
        .spawn(|| {
            let latest = match get_latest_release_info() {
                Ok(latest) => latest,
                Err(err) => {
                    log::warn!("manual update check failed: {:#}", err);
                    toast(
                        &format!("{} update check failed", crate::brand::PRODUCT_NAME),
                        &format!("{:#}", err),
                    );
                    return;
                }
            };

            cache_release(&latest);
            schedule_set_banner_from_release_info(&latest);

            let current = wezterm_version();
            if !is_release_build(current) {
                // Comparing a `20260730-121314-abc12345` dev build against a
                // `tgz-v2026.07.2` tag numerically is meaningless, so say so
                // rather than claiming to be up to date.
                notify_update_available(&latest);
                log::info!(
                    "development build {}; latest release is {}",
                    current,
                    latest.tag_name
                );
            } else if release_tag_is_newer(latest.tag_name.as_str(), current) {
                notify_update_available(&latest);
            } else {
                toast(
                    &format!("{} is up to date", crate::brand::PRODUCT_NAME),
                    &format!("{} is the latest release", current),
                );
            }
        })
    {
        log::warn!("unable to spawn update check thread: {:#}", err);
    }
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

                    if force_ui || socks.is_empty() || socks[0] == my_sock {
                        notify_update_available(&latest);
                    }
                }

                // Record the time of this check
                cache_release(&latest);
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
    use super::{is_release_build, pick_asset_for, release_tag_is_newer, Asset, Platform, Release};

    fn release_with(asset_names: &[&str]) -> Release {
        Release {
            url: "https://api.github.com/repos/example/example/releases/1".to_string(),
            body: String::new(),
            html_url: "https://github.com/example/example/releases/tag/tgz-v2026.07.2".to_string(),
            tag_name: "tgz-v2026.07.2".to_string(),
            assets: asset_names
                .iter()
                .map(|name| Asset {
                    name: name.to_string(),
                    size: 1,
                    url: format!("https://api.github.com/assets/{}", name),
                    browser_download_url: format!("https://example.invalid/{}", name),
                })
                .collect(),
        }
    }

    /// The default branding; tests assert against the shipped asset names.
    const PRODUCT: &str = crate::brand::PRODUCT_NAME;

    /// A release as the workflows publish it today: versioned installer,
    /// versioned portable zip, the dmg, and a checksum beside each.
    fn full_release() -> Release {
        release_with(&[
            &format!("{}-windows-portable-tgz-v2026.08.5.zip", PRODUCT),
            &format!("{}-windows-portable-tgz-v2026.08.5.zip.sha256", PRODUCT),
            &format!("{}-Setup-tgz-v2026.08.5.exe", PRODUCT),
            &format!("{}-Setup-tgz-v2026.08.5.exe.sha256", PRODUCT),
            &format!("{}.dmg", PRODUCT),
        ])
    }

    #[test]
    fn macos_picks_the_dmg() {
        let release = full_release();
        let asset = pick_asset_for(&release, Platform::MacOS, false).expect("dmg should be picked");
        assert_eq!(asset.name, format!("{}.dmg", PRODUCT));
    }

    #[test]
    fn installed_windows_build_picks_the_versioned_installer() {
        let release = full_release();
        let asset =
            pick_asset_for(&release, Platform::Windows, false).expect("installer should be picked");
        assert_eq!(asset.name, format!("{}-Setup-tgz-v2026.08.5.exe", PRODUCT));
    }

    #[test]
    fn portable_windows_build_picks_the_zip() {
        // Handing a portable user an installer would replace their extracted
        // folder with an installed app, which is not what they asked for.
        let release = full_release();
        let asset =
            pick_asset_for(&release, Platform::Windows, true).expect("zip should be picked");
        assert_eq!(
            asset.name,
            format!("{}-windows-portable-tgz-v2026.08.5.zip", PRODUCT)
        );
    }

    #[test]
    fn each_windows_kind_falls_back_to_the_other() {
        let installer_only = release_with(&[&format!("{}-Setup-tgz-v2026.08.5.exe", PRODUCT)]);
        assert!(pick_asset_for(&installer_only, Platform::Windows, true).is_some());

        let zip_only = release_with(&[&format!("{}-windows-portable-tgz-v2026.08.5.zip", PRODUCT)]);
        assert!(pick_asset_for(&zip_only, Platform::Windows, false).is_some());
    }

    #[test]
    fn checksum_sidecars_are_never_offered_as_downloads() {
        let sidecars_only = release_with(&[
            &format!("{}-Setup-tgz-v2026.08.5.exe.sha256", PRODUCT),
            &format!("{}-windows-portable-tgz-v2026.08.5.zip.sha256", PRODUCT),
            &format!("{}.dmg.sha256", PRODUCT),
        ]);
        assert!(pick_asset_for(&sidecars_only, Platform::Windows, false).is_none());
        assert!(pick_asset_for(&sidecars_only, Platform::Windows, true).is_none());
        assert!(pick_asset_for(&sidecars_only, Platform::MacOS, false).is_none());
    }

    #[test]
    fn no_asset_for_unpublished_platforms() {
        assert!(pick_asset_for(&full_release(), Platform::Other, false).is_none());
    }

    #[test]
    fn no_asset_when_the_release_has_none() {
        let release = release_with(&[]);
        assert!(pick_asset_for(&release, Platform::MacOS, false).is_none());
        assert!(pick_asset_for(&release, Platform::Windows, false).is_none());
    }

    /// Guards against matching an unrelated asset that merely starts with the
    /// product name.
    #[test]
    fn unrelated_assets_are_not_picked() {
        let release = release_with(&[
            &format!("{}-debug-symbols.zip", PRODUCT),
            &format!("{}.dmg.sha256", PRODUCT),
        ]);
        assert!(pick_asset_for(&release, Platform::MacOS, false).is_none());
        assert!(pick_asset_for(&release, Platform::Windows, false).is_none());
    }

    #[test]
    fn release_builds_are_distinguished_from_dev_builds() {
        assert!(is_release_build("tgz-v2026.07.2"));
        assert!(!is_release_build("20260730-121314-abc12345"));
    }

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
