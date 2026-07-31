//! Keep the Web Inspector out of the hole-punch layout.
//!
//! WebKit's inspector opens *attached* by default: it re-parents into the inspected view's
//! superview and reframes the inspected view from that **superview's** bounds. curator's content
//! webviews are `add_child` siblings positioned over the chrome's content hole, and they composite
//! above the chrome, so an attached inspector stretches the inspected tab across the whole window
//! — measured: a tab at `(240,0) 1260×1000` became `(0,0) 1500×500`. The sidebar isn't gone, it's
//! covered, which is why it also stops taking clicks.
//!
//! Forcing the inspector into its own window is the fix, not a workaround: an attached inspector
//! has no way to express "share the window with a sidebar it doesn't know about". Detaching alone
//! isn't enough — WebKit restores the view to fill the superview rather than to the hole it came
//! from — so [`crate::commands::open_active_devtools`] re-applies the hole layout afterwards.
//! Re-docking the inspector from its own dock buttons reintroduces the reframe (nothing observes
//! that); ⌥⌘I puts it back in its own window and restores the layout.

/// Force the inspector attached to `webview` into its own window. No-op when it is already
/// detached, when the inspector doesn't exist yet, or on a WebKit without the SPI.
#[cfg(target_os = "macos")]
pub fn force_detached(webview_ptr: *mut std::ffi::c_void) {
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel};
    unsafe {
        let Some(view) = (webview_ptr as *mut AnyObject).as_ref() else {
            return;
        };
        // `_inspector` and `-detach` are WebKit SPI (the same `_inspector` wry's `open_devtools`
        // uses). Guard both on respondsToSelector: a WebKit that renames them then degrades to
        // "the inspector stays attached" instead of aborting on an unrecognised selector.
        let has_inspector: bool = msg_send![view, respondsToSelector: sel!(_inspector)];
        if !has_inspector {
            return;
        }
        let inspector: *mut AnyObject = msg_send![view, _inspector];
        let Some(inspector) = inspector.as_ref() else {
            return;
        };
        let can_detach: bool = msg_send![inspector, respondsToSelector: sel!(detach)];
        if can_detach {
            let _: () = msg_send![inspector, detach];
        }
    }
}
