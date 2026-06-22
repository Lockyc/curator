//! Bring a content webview to the front of its superview without hiding the others — the
//! covered webviews stay attached and `visible`, so their JS (chat /sync loops) keep running.

#[cfg(target_os = "macos")]
pub fn raise_to_front(webview_ptr: *mut std::ffi::c_void) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    const NS_WINDOW_ABOVE: isize = 1; // NSWindowOrderingMode::Above
    unsafe {
        let Some(view) = (webview_ptr as *mut AnyObject).as_ref() else {
            return;
        };
        let superview: *mut AnyObject = msg_send![view, superview];
        if superview.is_null() {
            return;
        }
        // addSubview:positioned:relativeTo: with a nil sibling reorders `view` to the top.
        let _: () = msg_send![
            superview,
            addSubview: view,
            positioned: NS_WINDOW_ABOVE,
            relativeTo: std::ptr::null::<AnyObject>()
        ];
    }
}
