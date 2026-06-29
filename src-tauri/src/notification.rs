//! Fire native macOS banner notifications, Rust-side. Called from the `on_navigation`
//! notify-sentinel handler — so notifications never require granting a remote service webview
//! any command/IPC capability.
//!
//! Posted via the **UserNotifications** framework (`UNUserNotificationCenter`), not
//! `tauri-plugin-notification`: that plugin's desktop backend (`notify-rust` →
//! `mac-notification-sys`) posts via the deprecated `NSUserNotification` API, which is a silent
//! no-op on macOS 26 — `show()` returns `Ok`, nothing is delivered, and the app never even
//! registers in Notification Center. `UNUserNotificationCenter` is the modern, supported path and
//! posts under curator's own bundle identity. Two requirements it imposes, both handled in
//! [`init`]: request authorization once, and install a delegate that opts banners in even while
//! curator is the frontmost app (otherwise the system suppresses the banner whenever the posting
//! app is foreground — exactly curator's hidden-tab-in-the-focused-window case).

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread};
    use objc2_foundation::{NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };

    /// True once authorization has been requested and the delegate installed — i.e. native banners
    /// are usable. Stays false in dev (`cargo run` / `just dev`): there is no app bundle, and
    /// `UNUserNotificationCenter::currentNotificationCenter` throws on a nil bundle identifier.
    static BANNER_READY: AtomicBool = AtomicBool::new(false);

    /// Monotonic suffix for request identifiers, so distinct alerts don't coalesce (a reused
    /// identifier *replaces* the pending request rather than stacking a new banner).
    static BANNER_SEQ: AtomicU64 = AtomicU64::new(0);

    define_class!(
        // A `UNUserNotificationCenterDelegate` whose sole job is to present banners even while
        // curator is the frontmost app. Without it, the system silently drops the banner (showing
        // it only in the Notification Center list) whenever the posting app is foreground — which
        // is exactly the hidden-tab-in-the-focused-window case curator notifies on.
        #[unsafe(super(NSObject))]
        #[name = "CuratorNotificationDelegate"]
        struct NotificationDelegate;

        unsafe impl NSObjectProtocol for NotificationDelegate {}

        unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                let opts = UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::Sound;
                completion_handler.call((opts,));
            }
        }
    );

    /// Request notification authorization once and install the presentation delegate. No-op in dev
    /// (no bundle → `currentNotificationCenter` would throw); native banners are only expected from
    /// the packaged `curator.app`. Runs on the main thread (Tauri's setup hook).
    pub fn init() {
        if tauri::is_dev() {
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();

        // The center holds its delegate weakly, so the instance must outlive setup — leak it as an
        // app-lifetime singleton (it carries no state and is never torn down before exit).
        let delegate: Retained<NotificationDelegate> =
            unsafe { msg_send![NotificationDelegate::alloc(), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        std::mem::forget(delegate);

        let opts = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        // Async; the system shows its one-time prompt on first launch. Heap block (RcBlock)
        // because it escapes the call.
        let handler = RcBlock::new(|granted: Bool, _err: *mut NSError| {
            if !granted.as_bool() {
                eprintln!(
                    "curator: notification authorization not granted — banners will be suppressed"
                );
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(opts, &handler);

        BANNER_READY.store(true, Ordering::Release);
    }

    /// Show a native banner. No-op until [`init`] has run (dev, or before setup). Main thread only.
    pub fn fire(title: &str, body: &str) {
        if !BANNER_READY.load(Ordering::Acquire) {
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::init(UNMutableNotificationContent::alloc());
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));

        let id = BANNER_SEQ.fetch_add(1, Ordering::Relaxed);
        let ident = NSString::from_str(&format!("curator-{id}"));
        // nil trigger → deliver immediately; nil completion handler → fire-and-forget.
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&ident, &content, None);
        center.addNotificationRequest_withCompletionHandler(&request, None);
    }
}

#[cfg(target_os = "macos")]
pub use imp::{fire, init};

/// Non-macOS stubs so the rest of the crate compiles off-platform (curator is macOS-only, but the
/// config/CLI paths build anywhere).
#[cfg(not(target_os = "macos"))]
pub fn init() {}

#[cfg(not(target_os = "macos"))]
pub fn fire(_title: &str, _body: &str) {}
