//! Make the active content tab the sole cursor authority over the content hole.
//!
//! **Why this exists.** curator's hole-punch stacks two WKWebViews: the chrome is the window's
//! full-window MAIN webview (it must be — `data-tauri-drag-region` only moves the window from the
//! main webview) and each content tab is an `add_child` sibling composited above it over the hole.
//! AppKit's mouse tracking is **geometric, not occlusion-aware**, so the chrome keeps claiming the
//! hole region even while a tab is painted on top of it, and *both* webviews compute a cursor for the
//! same pixel. WebKit pushes its cursor asynchronously over IPC while AppKit re-asserts synchronously
//! per mouse event, so the two race: the pointing hand over a link flickers while the pointer moves
//! and reverts to the arrow the moment it stops.
//!
//! That stacking is as old as the hole-punch — what changed is the OS. Verified by running old tags
//! on the current system: v0.7.2 (which predates the update) flickers identically to v0.10.0, so only
//! macOS changed, not this repo. See `docs/FOLLOWUPS.md` for the bisect and the full ruled-out list;
//! **cursor rects and hit-testing are both dead ends** — WebKit derives its cursor from neither, so
//! `disableCursorRects` and a `hitTest:` override each measurably changed nothing.
//!
//! **How the fix works.** The only interceptable point is the cursor call itself, so we swizzle
//! `-[NSCursor set]` and drop the two spurious sources:
//!
//! 1. **The covered chrome.** A cursor set can't otherwise be attributed to a webview, so the chrome
//!    tags its own: `#content-hole` is styled with a sentinel cursor (`SENTINEL_CSS`) that nothing
//!    else in either webview uses. A sentinel set therefore *is* "the covered chrome asked for this".
//!    We drop it when a content tab is under the pointer, and substitute the arrow when the hole is
//!    bare (no tab loaded) so the empty state still gets a sane cursor.
//! 2. **AppKit's own arrow.** `-[_NSTrackingAreaAKManager setCursorForMouseLocation:]` re-asserts the
//!    arrow per mouse event; measured at 33 arrow-sets against WebKit's 31 hands over the same pixels.
//!    Suppressed by checking the calling module, so WebKit-originated arrows (the genuine ones, over
//!    plain page text) still pass through.
//!
//! Everything else passes through untouched, so the sidebar's own cursors — including the resize
//! handle's `col-resize` — are unaffected. The sentinel is deliberately `context-menu`: real pages
//! effectively never set it, and it is distinct from every cursor the sidebar uses.
//!
//! **Known limitation:** a page that genuinely sets `cursor: context-menu` gets it suppressed over
//! the content area. That is the price of attributing cursor sets at all, and it is the rarest CSS
//! cursor to collide on.
//!
//! **Footgun — never isa-swizzle the webview instance to solve cursor problems.** tauri/wry already
//! KVO-swizzles the webview, so its real class is an `NSKVONotifying_…` subclass; `object_setClass`
//! underneath KVO aborts at launch with `Assertion failed: (imp != NULL) … NSDynamicProperties.m`.
//! Swizzling `NSCursor` (as here) is unaffected — `NSCursor` is not KVO'd by the webview stack.

/// The CSS cursor the chrome stamps on `#content-hole` to tag its own cursor sets — the CSS-side
/// name of the same cursor [`install`] resolves natively via `contextualMenuCursor`. Exists to pin
/// the lockstep with `src/chrome.css`: the arbiter can only attribute a set if the chrome actually
/// asks for this, so the pairing is asserted by a test rather than left to a comment.
#[cfg(all(test, target_os = "macos"))]
pub(crate) const SENTINEL_CSS: &str = "context-menu";

#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use std::os::raw::{c_char, c_void};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(target_os = "macos")]
static ORIG_SET: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_os = "macos")]
static ARROW: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_os = "macos")]
static SENTINEL: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_os = "macos")]
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Registered chrome webviews (one per window), so the arbiter can tell "the hole is bare" from
/// "a content tab covers this pixel". Lock-free: consulted on every cursor set.
#[cfg(target_os = "macos")]
const MAX_CHROME: usize = 32;
#[cfg(target_os = "macos")]
static CHROME: [AtomicUsize; MAX_CHROME] = [const { AtomicUsize::new(0) }; MAX_CHROME];

#[cfg(target_os = "macos")]
#[repr(C)]
struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn backtrace(buffer: *mut *mut c_void, size: i32) -> i32;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> i32;
}

/// `true` when the nearest identifiable caller above us is AppKit (its tracking-area cursor manager)
/// rather than WebKit — i.e. this is AppKit re-asserting, not a page's genuine cursor.
#[cfg(target_os = "macos")]
unsafe fn called_by_appkit() -> bool {
    let mut frames: [*mut c_void; 8] = [std::ptr::null_mut(); 8];
    let n = backtrace(frames.as_mut_ptr(), 8);
    // [0] = backtrace, [1] = this fn, [2..] = the real callers.
    for frame in frames.iter().take((n as usize).min(8)).skip(2) {
        let mut info = DlInfo {
            dli_fname: std::ptr::null(),
            dli_fbase: std::ptr::null_mut(),
            dli_sname: std::ptr::null(),
            dli_saddr: std::ptr::null_mut(),
        };
        if dladdr(*frame, &mut info) != 0 && !info.dli_fname.is_null() {
            let name = std::ffi::CStr::from_ptr(info.dli_fname).to_string_lossy();
            if name.contains("/AppKit") {
                return true;
            }
            if name.contains("/WebKit") || name.contains("/WebCore") {
                return false;
            }
        }
    }
    false
}

/// Whether a content tab currently covers the pointer — i.e. the view under the mouse is something
/// other than this window's chrome. When the hole is bare the chrome itself is the hit view.
#[cfg(target_os = "macos")]
unsafe fn content_tab_under_pointer() -> bool {
    use objc2::{class, msg_send};
    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    if app.is_null() {
        return false;
    }
    let win: *mut AnyObject = msg_send![app, keyWindow];
    if win.is_null() {
        return false;
    }
    let point: objc2_foundation::NSPoint = msg_send![win, mouseLocationOutsideOfEventStream];
    let content_view: *mut AnyObject = msg_send![win, contentView];
    if content_view.is_null() {
        return false;
    }
    let hit: *mut AnyObject = msg_send![content_view, hitTest: point];
    if hit.is_null() {
        return false;
    }
    let hit_addr = hit as usize;
    // A registered chrome under the pointer means the hole is bare here.
    !CHROME.iter().any(|c| c.load(Ordering::Relaxed) == hit_addr)
}

#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn swizzled_set(this: *mut AnyObject, cmd: objc2::runtime::Sel) {
    let call_original = |cursor: *mut AnyObject| {
        let orig = ORIG_SET.load(Ordering::Relaxed);
        if orig != 0 {
            let f: unsafe extern "C-unwind" fn(*mut AnyObject, objc2::runtime::Sel) =
                std::mem::transmute(orig);
            f(cursor, cmd);
        }
    };

    let addr = this as usize;

    // 1. The covered chrome tagged this set as its own.
    if addr == SENTINEL.load(Ordering::Relaxed) {
        if content_tab_under_pointer() {
            return; // the tab owns this pixel — let its cursor stand
        }
        call_original(ARROW.load(Ordering::Relaxed) as *mut AnyObject);
        return;
    }

    // 2. AppKit re-asserting the arrow over a cursor WebKit already set.
    if addr == ARROW.load(Ordering::Relaxed) && called_by_appkit() {
        return;
    }

    call_original(this);
}

/// Register a window's **chrome** webview. Call once per window.
#[cfg(target_os = "macos")]
pub fn register_chrome(webview_ptr: *mut std::ffi::c_void) {
    let v = webview_ptr as usize;
    if v == 0 || CHROME.iter().any(|c| c.load(Ordering::Relaxed) == v) {
        return;
    }
    for slot in CHROME.iter() {
        if slot
            .compare_exchange(0, v, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
    }
}

/// Install the cursor arbiter. Call once from the Tauri setup hook, on the main thread.
#[cfg(target_os = "macos")]
pub fn install() {
    use objc2::runtime::Imp;
    use objc2::{class, msg_send, sel};
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        let cls = class!(NSCursor);
        let arrow: *mut AnyObject = msg_send![cls, arrowCursor];
        let sentinel: *mut AnyObject = msg_send![cls, contextualMenuCursor];
        ARROW.store(arrow as usize, Ordering::Relaxed);
        SENTINEL.store(sentinel as usize, Ordering::Relaxed);
        let Some(method) = cls.instance_method(sel!(set)) else {
            return;
        };
        let imp: Imp = std::mem::transmute(
            swizzled_set as unsafe extern "C-unwind" fn(*mut AnyObject, objc2::runtime::Sel),
        );
        let prev = method.set_implementation(imp);
        ORIG_SET.store(prev as usize, Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}

#[cfg(not(target_os = "macos"))]
pub fn register_chrome(_webview_ptr: *mut std::ffi::c_void) {}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::SENTINEL_CSS;

    /// The arbiter can only drop the covered chrome's cursor sets if the chrome actually *makes*
    /// them — i.e. `#content-hole` really carries the sentinel. Drop the rule (or change it on one
    /// side only) and the tagging silently stops working: no error, no failing build, just the link
    /// cursor flickering again. This is that lockstep, enforced.
    #[test]
    fn chrome_css_stamps_the_sentinel_cursor_on_the_content_hole() {
        let css = include_str!("../../src/chrome.css");
        let hole_rule = css
            .lines()
            .find(|l| l.trim_start().starts_with("#content-hole {"))
            .expect("chrome.css must style #content-hole");
        assert!(
            hole_rule.contains(&format!("cursor: {SENTINEL_CSS}")),
            "#content-hole must set `cursor: {SENTINEL_CSS}` so cursor_arbiter can attribute the \
             covered chrome's cursor sets; found: {hole_rule}"
        );
    }
}
