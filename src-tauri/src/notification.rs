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
//!
//! The banner is also **clickable**: [`fire`] stamps the originating `(window id, tab label)` into
//! the request's `userInfo`, and the delegate's `did_receive` reads it back on a tap to raise that
//! window and tell its chrome to select the tab — so a notification surfaces the tab that raised
//! it, even from the background. (This is curator-level tab surfacing; the page's own web
//! `Notification.onclick` stub is still not invoked — see `src/inject/notification.js`.)

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::OnceLock;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread};
    use objc2_foundation::{NSDictionary, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
        UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };
    use tauri::{AppHandle, Emitter, Manager};

    /// True once authorization has been requested and the delegate installed — i.e. native banners
    /// are usable. Stays false in dev (`cargo run` / `just dev`): there is no app bundle, and
    /// `UNUserNotificationCenter::currentNotificationCenter` throws on a nil bundle identifier.
    static BANNER_READY: AtomicBool = AtomicBool::new(false);

    /// The app handle, captured at [`init`]. The notification-click delegate (`did_receive`) runs on
    /// the main thread with no context of its own, so it reaches the window + chrome through this.
    static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

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

            // The user clicked (or dismissed) a banner. On a click — the *default* action — surface
            // the tab that raised it: read the (window id, tab label) stamped into `userInfo`, raise
            // that window, and tell its chrome to select the tab. Delivered on the main thread, so
            // touching Tauri/AppKit here is safe. Always call the completion handler (required).
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion_handler: &block2::DynBlock<dyn Fn()>,
            ) {
                // Ignore dismiss / custom actions — only a tap on the banner body surfaces the tab.
                if &*response.actionIdentifier() == unsafe { UNNotificationDefaultActionIdentifier }
                {
                    let user_info = response.notification().request().content().userInfo();
                    if let Some(window_id) = dict_str(&user_info, "label") {
                        focus_window_tab(window_id, dict_str(&user_info, "id"));
                    }
                }
                completion_handler.call(());
            }
        }
    );

    /// Read a string value out of a notification's `userInfo`. The dictionary is untyped
    /// (`NSDictionary<AnyObject, AnyObject>`); we stamped only `NSString → NSString` pairs into it
    /// ([`fire`]), so casting to that concrete type and looking the key up is sound.
    fn dict_str(user_info: &NSDictionary, key: &str) -> Option<String> {
        // SAFETY: every value we put in is an `NSString` keyed by an `NSString` (see `fire`).
        let typed: &NSDictionary<NSString, NSString> = unsafe {
            &*(user_info as *const NSDictionary as *const NSDictionary<NSString, NSString>)
        };
        typed
            .objectForKey(&NSString::from_str(key))
            .map(|s| s.to_string())
    }

    /// Raise the window owning a clicked banner and ask its chrome to select the tab. Best-effort:
    /// an unknown window id (since closed) raises nothing; a missing/stale tab label leaves the
    /// active tab alone, so the click still lands you in the right window. `set_focus` brings the
    /// window forward and activates curator, so this surfaces the tab even from the background.
    /// The event targets the originating window's chrome webview label (`{window_id}:chrome`)
    /// directly, so it reaches only that window's sidebar.
    fn focus_window_tab(window_id: String, tab_label: Option<String>) {
        let Some(app) = APP_HANDLE.get() else {
            return;
        };
        if let Some(win) = app.get_window(&window_id) {
            let _ = win.unminimize();
            let _ = win.set_focus();
        }
        if let Some(tab) = tab_label {
            let _ = app.emit_to(
                crate::identity::namespaced(&window_id, "chrome"),
                "focus-tab",
                serde_json::json!({ "label": tab }),
            );
        }
    }

    /// Request notification authorization once and install the presentation/click delegate, and
    /// capture the `AppHandle` the click delegate needs to raise a window + emit to its chrome.
    /// No-op for the *banner* path in dev (no bundle → `currentNotificationCenter` would throw);
    /// native banners are only expected from the packaged `curator.app`. Runs on the main thread
    /// (Tauri's setup hook).
    pub fn init(app: AppHandle) {
        let _ = APP_HANDLE.set(app);
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
    /// `window_id`/`tab` are stamped into the request's `userInfo` so a click can route back to the
    /// originating window + tab (`did_receive`).
    pub fn fire(title: &str, body: &str, window_id: &str, tab: &str) {
        if !BANNER_READY.load(Ordering::Acquire) {
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::init(UNMutableNotificationContent::alloc());
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));

        // Stamp the originating (window id, tab label) so a banner click surfaces the right tab.
        let keys = [NSString::from_str("label"), NSString::from_str("id")];
        let vals = [NSString::from_str(window_id), NSString::from_str(tab)];
        let user_info: Retained<NSDictionary<NSString, NSString>> =
            NSDictionary::from_retained_objects(&[&*keys[0], &*keys[1]], &vals);
        // setUserInfo wants the untyped NSDictionary<AnyObject, AnyObject>; the typed dict has the
        // same layout (phantom key/value types), so the cast is sound.
        // SAFETY: only the phantom generic params differ; the Objective-C object is identical.
        let user_info: Retained<NSDictionary> = unsafe { Retained::cast_unchecked(user_info) };
        // SAFETY: the dict holds only NSStrings (property-list types), valid notification userInfo.
        unsafe { content.setUserInfo(&user_info) };

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
pub fn init(_app: tauri::AppHandle) {}

#[cfg(not(target_os = "macos"))]
pub fn fire(_title: &str, _body: &str, _window_id: &str, _tab: &str) {}
