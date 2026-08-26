#![cfg(target_os = "macos")]
use crate::click::{self, ResponseKind};
use crate::{ToastClick, ToastNotification};
use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDismissActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::sync::{LazyLock, Once};
use std::time::Instant;

/// Action + category for a notification whose click focuses something inside
/// this app rather than opening a url. Separate from `SHOW_URL_ACTION` because
/// the two mean different things, and `setNotificationCategories` replaces the
/// whole set, so both have to be registered together.
const FOCUS_ACTION: &str = "FOCUS_TARGET";
const FOCUS_CATEGORY: &str = "FOCUS_TARGET_ACTION";

const NEEDS_SIGN: &str = "Note that the application must be code-signed \
                          for UNUserNotificationCenter to work";

fn ns_error_to_string(err: *mut NSError) -> String {
    if err.is_null() {
        "null error".to_string()
    } else {
        unsafe {
            let err: &NSError = &*err;
            format!(
                "{} {:?}",
                err.localizedDescription(),
                err.localizedFailureReason()
            )
        }
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[name = "WezTermNotifDelegate"]
    #[derive(Debug)]
    struct NotifDelegate;

    unsafe impl NSObjectProtocol for NotifDelegate {}
    unsafe impl UNUserNotificationCenterDelegate for NotifDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        unsafe fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &block2::Block<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            log::debug!("will_present");
            let options = UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound
                | UNNotificationPresentationOptions::Badge
                | UNNotificationPresentationOptions::Banner;
            completion_handler.call((options,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        unsafe fn did_receive_notification(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &Block<dyn Fn()>,
        ) {
            let action = response.actionIdentifier().to_string();
            let request = response.notification().request();
            let identifier = request.identifier().to_string();
            let user_info = request.content().userInfo();
            let url = user_info.valueForKey(ns_string!("url"));

            log::debug!(
                "did_receive_notification -> action={action:?} id={identifier} url={url:?}"
            );

            let dismiss = UNNotificationDismissActionIdentifier.to_string();
            match click::classify_action(&action, &dismiss) {
                ResponseKind::Dismiss => {
                    // Swiping a notification away is not a request to go
                    // anywhere; drop the parked handler unfired.
                    click::with(|registry| registry.forget(&identifier));
                }
                ResponseKind::Activate => {
                    // Take the handler out from under the lock before running
                    // it: a handler is free to post another notification, and
                    // doing that while holding the registry would deadlock.
                    let handler = click::with(|registry| registry.take(&identifier));
                    match handler {
                        Some(handler) => handler(),
                        None => {
                            log::debug!("no click handler parked for {identifier}");
                        }
                    }

                    if let Some(url) = url {
                        if let Ok(url_str) = url.downcast::<NSString>() {
                            wezterm_open_url::open_url(&url_str.to_string());
                        }
                    }
                }
            }

            completion_handler.call(());
        }
    }
);

impl NotifDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        let me: Retained<Self> = unsafe { msg_send![super(this), init] };
        log::debug!("new delegate {:?}", Retained::as_ptr(&me));
        me
    }
}

impl Drop for NotifDelegate {
    fn drop(&mut self) {
        log::debug!("dropping NotifDelegate {:?}", self as *mut Self);
    }
}

const CENTER: LazyLock<Retained<UNUserNotificationCenter>> =
    LazyLock::new(|| unsafe { UNUserNotificationCenter::currentNotificationCenter() });

pub fn initialize() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        CENTER.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Provisional
                | UNAuthorizationOptions::Sound,
            &RcBlock::new(|ok: Bool, err| {
                if ok.is_false() {
                    log::error!(
                        "requestAuthorization status={ok:?} {}. {NEEDS_SIGN}",
                        ns_error_to_string(err)
                    );
                }
            }),
        );

        let show_url = UNNotificationAction::actionWithIdentifier_title_options(
            ns_string!("SHOW_URL"),
            ns_string!("Show"),
            UNNotificationActionOptions::empty(),
        );
        let show_url_cat =
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                ns_string!("SHOW_URL_ACTION"),
                &NSArray::from_retained_slice(&[show_url]),
                &NSArray::from_slice(&[]),
                UNNotificationCategoryOptions::CustomDismissAction,
            );
        // `Foreground` is what brings the app forward when the button is used.
        let focus_target = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(FOCUS_ACTION),
            ns_string!("Show"),
            UNNotificationActionOptions::Foreground,
        );
        let focus_target_cat =
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(FOCUS_CATEGORY),
                &NSArray::from_retained_slice(&[focus_target]),
                &NSArray::from_slice(&[]),
                // Report swipe-aways, so a dismissed notification frees its
                // parked handler instead of waiting to be evicted.
                UNNotificationCategoryOptions::CustomDismissAction,
            );
        CENTER.setNotificationCategories(&NSSet::from_retained_slice(&[
            show_url_cat,
            focus_target_cat,
        ]));

        let delegate = NotifDelegate::new();
        let delegate_proto = ProtocolObject::from_retained(delegate.clone());
        CENTER.setDelegate(Some(&delegate_proto));
        log::debug!(
            "after setDelegate {:?}, center.delegate={:?}",
            delegate,
            CENTER.delegate()
        );

        // Intentionally "leak" the delegate.
        // I've tried stashing it into a global to keep it alive,
        // but something still manages to drop the underlying delegate
        // and that will break the weak ref in the center.
        // This is likely not the right way to do this, but after
        // spending two hours scratching my head, this is the least
        // crazy thing.
        Retained::into_raw(delegate);
    });
}

pub fn show_notif(
    toast: ToastNotification,
    on_click: Option<ToastClick>,
) -> Result<(), Box<dyn std::error::Error>> {
    initialize();
    unsafe {
        log::debug!("show_notif center.delegate is {:?}", CENTER.delegate());

        // Minted up front: this is both the request id and the key the delegate
        // uses to find the click handler again.
        let identifier = uuid::Uuid::new_v4().to_string();
        let notif = UNMutableNotificationContent::new();
        notif.setTitle(&NSString::from_str(&toast.title));
        notif.setBody(&NSString::from_str(&toast.message));

        if let Some(url) = &toast.url {
            let info =
                NSDictionary::from_slices(&[ns_string!("url")], &[&*NSString::from_str(&url)]);
            notif.setUserInfo(
                info.downcast_ref::<NSDictionary>()
                    .expect("is NSDictionary"),
            );
            notif.setCategoryIdentifier(ns_string!("SHOW_URL_ACTION"));
        } else if on_click.is_some() {
            notif.setCategoryIdentifier(&NSString::from_str(FOCUS_CATEGORY));
        }

        if let Some(on_click) = on_click {
            click::with(|registry| registry.insert(identifier.clone(), on_click, Instant::now()));
        }

        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&identifier),
            &*notif,
            None,
        );

        CENTER.addNotificationRequest_withCompletionHandler(
            &*request,
            Some(&RcBlock::new(move |err: *mut NSError| {
                if err.is_null() {
                    if let Some(timeout) = toast.timeout {
                        let expiring = identifier.clone();
                        // Spawn a thread to wait. This could be more efficient.
                        // We cannot simply use performSelector:withObject:afterDelay:
                        // because we're not guaranteed to be called from the main
                        // thread.  We also don't have access to the executor machinery
                        // from the window crate here, so we just do this basic take.
                        let identifier = identifier.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(timeout);
                            // Remove this notification
                            let ident_array =
                                NSArray::from_retained_slice(&[NSString::from_str(&identifier)]);
                            CENTER.removeDeliveredNotificationsWithIdentifiers(&ident_array);
                            // The banner is gone, so nothing can click it now.
                            click::with(|registry| registry.forget(&expiring));
                        });
                    }
                } else {
                    log::error!("notif failed {}. {NEEDS_SIGN}", ns_error_to_string(err));
                    click::with(|registry| registry.forget(&identifier));
                }
            })),
        );
    }

    Ok(())
}
