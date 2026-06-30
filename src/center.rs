//! Center a window on its monitor.
//!
//! GTK4 removed programmatic window positioning; on X11 we move the underlying
//! X window directly via XMoveWindow once the surface is realized.

use gtk4 as gtk;
use gtk::prelude::*;

/// Center `window` on whichever monitor it lands on, after it's mapped.
/// No-op on non-X11 backends (the surface downcast simply fails).
pub fn center_on_monitor(window: &impl IsA<gtk::Window>) {
    let window = window.clone().upcast::<gtk::Window>();
    // The surface only exists after the window is mapped; do it on `map`.
    window.connect_map(move |window| {
        if let Some(surface) = window.surface() {
            try_center_x11(window, &surface);
        }
    });
}

/// Force `window` to `w`x`h` via X11 once mapped. GTK4's `set_default_size` is a
/// no-op on a window already mapped at a smaller size (as happens here: the login
/// flow maps the main window empty before it has content), so resize the X
/// surface directly. No-op off X11.
pub fn resize_x11(window: &impl IsA<gtk::Window>, w: i32, h: i32) {
    use gdk4_x11::{X11Display, X11Surface};
    let window = window.clone().upcast::<gtk::Window>();
    let Some(surface) = window.surface() else { return };
    let Ok(x11_surface) = surface.clone().downcast::<X11Surface>() else { return };
    let Some(display) = surface.display().downcast::<X11Display>().ok() else { return };
    let xid = x11_surface.xid();
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else { return };
    // SAFETY: live Xlib Display for this surface; `xid` is its valid window ID.
    unsafe {
        let xdisplay = display.xdisplay();
        (xlib.XResizeWindow)(xdisplay, xid, w as u32, h as u32);
        (xlib.XFlush)(xdisplay);
    }
}

fn try_center_x11(window: &gtk::Window, surface: &gtk::gdk::Surface) {
    use gdk4_x11::{X11Display, X11Surface};

    let Ok(x11_surface) = surface.clone().downcast::<X11Surface>() else {
        return; // not X11 (e.g. Wayland) — leave placement to the compositor
    };
    let Some(display) = surface.display().downcast::<X11Display>().ok() else {
        return;
    };

    // Monitor geometry the window currently sits on.
    let geo = display
        .monitor_at_surface(surface)
        .map(|m| m.geometry())
        .unwrap_or_else(|| gtk::gdk::Rectangle::new(0, 0, 1920, 1080));

    // Window size: prefer the realized size, fall back to the default size.
    let (mut w, mut h) = (window.width(), window.height());
    if w <= 1 || h <= 1 {
        let (dw, dh) = window.default_size();
        w = dw.max(1);
        h = dh.max(1);
    }

    let x = geo.x() + (geo.width() - w) / 2;
    let y = geo.y() + (geo.height() - h) / 2;

    let xid = x11_surface.xid();
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else { return };
    // SAFETY: xdisplay() returns the live Xlib Display for this X11 surface, and
    // `xid` is that surface's valid X window ID. XMoveWindow only repositions it.
    unsafe {
        let xdisplay = display.xdisplay();
        (xlib.XMoveWindow)(xdisplay, xid, x, y);
        (xlib.XFlush)(xdisplay);
    }
}
