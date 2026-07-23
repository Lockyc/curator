//! Stop the chrome webview from owning the pointer where a content tab covers it.
//!
//! **The problem.** curator's hole-punch stacks two WKWebViews: the chrome is the window's
//! full-window MAIN webview, and each content tab is an `add_child` sibling composited above it over
//! the hole. AppKit's mouse tracking is **geometric, not occlusion-aware** — the chrome still claims
//! the hole region even while a content webview is painted on top of it. So both webviews compute a
//! cursor for the same pixel: the content tab says "pointing hand" over a link, the chrome says
//! whatever `#content-hole` resolves to (an arrow). WebKit pushes its cursor asynchronously over IPC
//! while AppKit re-asserts synchronously per mouse event, so the two race — the link cursor flickers
//! while the pointer moves and reverts to the arrow the moment it stops. Proven by styling
//! `#content-hole` with a distinctive cursor and watching it render *over* a live page.
//!
//! **The fix.** A view covered by a sibling should not claim the pointer there. We override
//! `hitTest:` so the chrome returns nil when the point falls inside a visible sibling stacked above
//! it — exactly when a content tab covers that pixel. Everywhere else (the sidebar, and the bare hole
//! when no tab is loaded) it defers to the original implementation, so the sidebar, the empty state,
//! and `data-tauri-drag-region` behave as before.
//!
//! **Footgun — do NOT implement this by isa-swizzling the webview instance.** The obvious approach
//! (build a runtime subclass and `object_setClass` the chrome) aborts at launch with
//! `Assertion failed: (imp != NULL) … NSDynamicProperties.m`: tauri/wry already KVO-swizzles the
//! webview, so its real class is an `NSKVONotifying_…` subclass, and re-pointing the isa underneath
//! KVO corrupts KVO's dynamic-property machinery. Instead we add the override to the **wry webview
//! class** itself. `hitTest:` is implemented by `NSView`, not by that class, so adding it there
//! overrides `NSView`'s for wry webviews *only* — never process-wide — and the KVO subclass inherits
//! it untouched. Content webviews are instances of the same class, so the implementation
//! short-circuits straight to `super` for any instance that isn't a registered chrome.
//!
//! The sibling test needs no hole-rect bookkeeping and no coordinate conversion: `hitTest:` receives
//! its point in the receiver's **superview** coordinates, the same space the siblings' `frame`s are
//! in, so the containment check stays correct across window resizes and sidebar drags.

#[cfg(target_os = "macos")]
use objc2::runtime::{AnyClass, AnyObject, Sel};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSPoint, NSRect};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

/// Registered chrome webviews (one per window). Lock-free: `hitTest:` runs on every mouse move for
/// every wry webview, so the non-chrome path must stay cheap.
#[cfg(target_os = "macos")]
const MAX_CHROME: usize = 32;
#[cfg(target_os = "macos")]
static CHROME: [AtomicUsize; MAX_CHROME] = [const { AtomicUsize::new(0) }; MAX_CHROME];

/// Superclass of the class we added `hitTest:` to — i.e. the implementation we defer to.
#[cfg(target_os = "macos")]
static SUPER_CLASS: AtomicPtr<AnyClass> = AtomicPtr::new(std::ptr::null_mut());
#[cfg(target_os = "macos")]
static INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
fn is_chrome(ptr: *mut AnyObject) -> bool {
    let v = ptr as usize;
    CHROME.iter().any(|s| s.load(Ordering::Relaxed) == v)
}

/// `hitTest:` — a chrome webview declines points a visible sibling stacked above it covers;
/// everything else behaves exactly as before.
#[cfg(target_os = "macos")]
extern "C" fn hit_test(this: *mut AnyObject, _cmd: Sel, point: NSPoint) -> *mut AnyObject {
    use objc2::msg_send;
    unsafe {
        if !this.is_null() && is_chrome(this) {
            let superview: *mut AnyObject = msg_send![this, superview];
            if !superview.is_null() {
                let subviews: *mut AnyObject = msg_send![superview, subviews];
                if !subviews.is_null() {
                    let count: usize = msg_send![subviews, count];
                    // Subviews are back-to-front: anything after us is stacked above us.
                    let mut above = false;
                    for i in 0..count {
                        let v: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
                        if v == this {
                            above = true;
                            continue;
                        }
                        if !above || v.is_null() {
                            continue;
                        }
                        let hidden: bool = msg_send![v, isHidden];
                        if hidden {
                            continue;
                        }
                        let f: NSRect = msg_send![v, frame];
                        if point.x >= f.origin.x
                            && point.x < f.origin.x + f.size.width
                            && point.y >= f.origin.y
                            && point.y < f.origin.y + f.size.height
                        {
                            // A content tab owns this pixel — don't claim it.
                            return std::ptr::null_mut();
                        }
                    }
                }
            }
        }
        let sup = SUPER_CLASS.load(Ordering::Relaxed);
        if this.is_null() || sup.is_null() {
            return std::ptr::null_mut();
        }
        msg_send![super(&*this, &*sup), hitTest: point]
    }
}

/// Register `webview_ptr` (a window's **chrome** WKWebView) as a view that should decline covered
/// points, installing the class-level override on first call. Call once per window.
#[cfg(target_os = "macos")]
pub fn decline_covered_points(webview_ptr: *mut std::ffi::c_void) {
    use objc2::ffi::class_addMethod;
    use objc2::runtime::Imp;
    use objc2::{msg_send, sel};

    // `@` return (NSView*), `@` self, `:` _cmd, `{CGPoint=dd}` point.
    const TYPES: &std::ffi::CStr = c"@@:{CGPoint=dd}";

    unsafe {
        let ptr = webview_ptr as *mut AnyObject;
        let Some(obj) = ptr.as_ref() else {
            return;
        };

        // Remember this chrome instance (idempotent).
        if !is_chrome(ptr) {
            let v = ptr as usize;
            for slot in CHROME.iter() {
                if slot
                    .compare_exchange(0, v, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    break;
                }
            }
        }

        if INSTALLED.swap(true, Ordering::Relaxed) {
            return;
        }
        // `-[NSObject class]` reports the ORIGINAL class, hiding any KVO subclass — exactly the class
        // we want the override on, so the KVO subclass inherits it.
        let cls: *const AnyClass = msg_send![obj, class];
        let Some(cls_ref) = cls.as_ref() else {
            return;
        };
        let Some(sup) = cls_ref.superclass() else {
            return;
        };
        SUPER_CLASS.store(sup as *const AnyClass as *mut AnyClass, Ordering::Relaxed);
        let imp: Imp = std::mem::transmute(
            hit_test as extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
        );
        class_addMethod(cls as *mut AnyClass, sel!(hitTest:), imp, TYPES.as_ptr());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn decline_covered_points(_webview_ptr: *mut std::ffi::c_void) {}
