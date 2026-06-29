//! Master-password login gate with doubling lockout.
//!
//! Shows a modal dialog before the main UI. On success it calls `on_unlock`
//! with the decrypted servers and the password (kept for re-saving).

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use age::secrecy::{ExposeSecret, SecretString};

use crate::store::{self, LoadError, Server};

/// Build and show the login window. `on_unlock` receives the loaded servers and
/// the master password once authentication succeeds.
pub fn show<F>(parent: &gtk::ApplicationWindow, on_unlock: F)
where
    F: Fn(Vec<Server>, SecretString) + 'static,
{
    let on_unlock = Rc::new(on_unlock);
    let first_run = !store::store_exists();

    // Modal transient of the (centered) main window so Mutter centers it too.
    let win = gtk::Window::builder()
        .title("Simple SSH Manager — Unlock")
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(360)
        .default_height(200)
        .resizable(false)
        .build();

    let heading = gtk::Label::new(Some(if first_run {
        "Set a master password to encrypt your servers."
    } else {
        "Enter master password."
    }));
    heading.set_wrap(true);

    let pw = gtk::PasswordEntry::builder().show_peek_icon(true).build();
    let status = gtk::Label::new(None);
    status.add_css_class("error");
    let unlock = gtk::Button::with_label(if first_run { "Create" } else { "Unlock" });
    unlock.add_css_class("suggested-action");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(16).margin_bottom(16).margin_start(16).margin_end(16)
        .build();
    content.append(&heading);
    content.append(&pw);
    content.append(&status);
    content.append(&unlock);
    win.set_child(Some(&content));

    // If currently locked out, disable entry and tick down a countdown.
    let gate = Rc::new(RefCell::new(()));
    let apply_lock = {
        let pw = pw.clone();
        let unlock = unlock.clone();
        let status = status.clone();
        move || -> bool {
            let remaining = store::lock_remaining();
            let locked = remaining > 0;
            pw.set_sensitive(!locked);
            unlock.set_sensitive(!locked);
            if locked {
                status.set_text(&format!("Locked. Try again in {remaining}s."));
            }
            locked
        }
    };
    // Tick once a second to re-enable when the lock expires.
    {
        let apply_lock = apply_lock.clone();
        let status = status.clone();
        glib::timeout_add_seconds_local(1, move || {
            if !apply_lock() {
                // unlocked again — clear the countdown text if it was a lock msg
                if status.text().starts_with("Locked") {
                    status.set_text("");
                }
            }
            glib::ControlFlow::Continue
        });
    }
    apply_lock();

    let attempt = {
        let pw = pw.clone();
        let status = status.clone();
        let win = win.clone();
        let on_unlock = on_unlock.clone();
        let apply_lock = apply_lock.clone();
        let gate = gate.clone();
        move || {
            let _g = gate.borrow_mut();
            if store::lock_remaining() > 0 {
                apply_lock();
                return;
            }
            let secret = SecretString::from(pw.text().to_string());
            if secret.expose_secret().is_empty() {
                status.set_text("Password cannot be empty.");
                return;
            }
            if first_run {
                // No file yet: this password becomes the master password.
                store::record_success();
                let _ = store::save(&[], &secret);
                win.close();
                on_unlock(Vec::new(), secret);
                return;
            }
            match store::load(&secret) {
                Ok(servers) => {
                    store::record_success();
                    win.close();
                    on_unlock(servers, secret);
                }
                Err(LoadError::WrongPassword) => {
                    let locked = store::record_failure();
                    pw.set_text("");
                    if locked > 0 {
                        apply_lock();
                    } else {
                        status.set_text("Wrong password.");
                    }
                }
                Err(LoadError::Corrupt) => status.set_text("Store is corrupt or unreadable."),
                Err(LoadError::Io(e)) => status.set_text(&format!("Read error: {e}")),
            }
        }
    };

    unlock.connect_clicked({
        let attempt = attempt.clone();
        move |_| attempt()
    });
    pw.connect_activate(move |_| attempt());

    crate::center::center_on_monitor(&win);
    win.present();
}

use gtk::glib;
