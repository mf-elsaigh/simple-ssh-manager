use std::cell::RefCell;
use std::rc::Rc;

mod center;
mod login;
mod store;
mod tray;

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{glib, gio, Application, ApplicationWindow};
use age::secrecy::SecretString;
use vte4::prelude::*;
use vte4::{PtyFlags, Terminal};

use store::Server;

const APP_ID: &str = "cloud.sadeem.SimpleSshManager";

/// Shared app state: the server list + the master password (for re-saving).
struct AppState {
    servers: Vec<Server>,
    password: SecretString,
}
type State = Rc<RefCell<AppState>>;

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        // Build the main window first (centered by the WM), then run the login
        // dialog modal-and-transient to it so it centers on the main window.
        // Use the installed themed icon (matches APP_ID).
        gtk::Window::set_default_icon_name(APP_ID);
        let window = ApplicationWindow::builder()
            .application(app)
            .title("Simple SSH Manager")
            .icon_name(APP_ID)
            .default_width(900)
            .default_height(600)
            .build();
        center::center_on_monitor(&window);
        // Note: not presented yet — it's shown in build_ui() once it has content,
        // so it opens at full size instead of an empty minimum. login centers
        // itself via center.rs and uses this (unmapped) window only as its parent.

        let window_for_unlock = window.clone();
        login::show(&window, move |servers, password| {
            let state: State = Rc::new(RefCell::new(AppState { servers, password }));
            build_ui(&window_for_unlock, state);
        });
    });
    app.run()
}

/// Populate the already-presented main window once the store is unlocked.
fn build_ui(window: &ApplicationWindow, state: State) {
    let notebook = gtk::Notebook::new();
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);

    // Sidebar: a ListBox where group headers and server rows coexist.
    let list = gtk::ListBox::new();
    list.set_vexpand(true);
    let scroll = gtk::ScrolledWindow::builder().child(&list).vexpand(true).build();

    let add_btn = gtk::Button::with_label("Add Server");

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 6);
    sidebar.set_width_request(220);
    sidebar.append(&scroll);
    sidebar.append(&add_btn);

    let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    split.append(&sidebar);
    split.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    split.append(&notebook);

    // Menu bar (Options) sits above the split.
    let menubar = build_menubar();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&menubar);
    root.append(&split);

    window.set_child(Some(&root));
    // Re-assert size: the window was presented empty (login covered it), so it
    // realized at its minimum. Restore the intended size now that it has content.
    window.set_default_size(900, 600);

    refresh_sidebar(&list, &state, window);
    install_menu_actions(window, &state, &list);

    // Open a tab when a server row is activated. Rows carry their server index
    // via the "server-index" data; group headers carry none.
    {
        let state = state.clone();
        let notebook = notebook.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(idx) = row_server_index(row) {
                if let Some(s) = state.borrow().servers.get(idx) {
                    open_terminal_tab(&notebook, s);
                }
            }
        });
    }

    {
        let state = state.clone();
        let list = list.clone();
        let window = window.clone();
        add_btn.connect_clicked(move |_| {
            show_edit_dialog(&window, state.clone(), list.clone(), None);
        });
    }

    install_tray_and_close(window, &notebook);

    // Present now that the window has content + size, so it opens full-sized.
    window.present();
    // The login flow maps this window empty first, so it realizes at its minimum
    // and set_default_size won't grow it back. Force the size on the X surface
    // once the present's map has produced a surface (next idle tick).
    glib::idle_add_local_once({
        let window = window.clone();
        move || center::resize_x11(&window, 900, 600)
    });
}

/// Minimize-to-tray + confirm-on-quit.
///
/// The window's close button (X) hides the window to the tray instead of
/// quitting — reversible, so no prompt. Actually quitting (tray "Quit",
/// Options -> Exit) goes through `confirm_quit`, which asks first.
fn install_tray_and_close(window: &ApplicationWindow, notebook: &gtk::Notebook) {
    use std::cell::Cell;
    // Shared flag: close-request hides to tray unless we're really quitting.
    let quitting = Rc::new(Cell::new(false));

    // Tray first: if there's no SNI host, the window X must NOT silently hide
    // (there'd be no way back) — it confirms-and-quits instead.
    let tray = tray::spawn(APP_ID);
    let has_tray = tray.is_some();

    {
        let quitting = quitting.clone();
        let notebook = notebook.clone();
        window.connect_close_request(move |w| {
            if quitting.get() {
                return glib::Propagation::Proceed; // let the window close -> app exits
            }
            if has_tray {
                w.set_visible(false); // minimize to tray
            } else {
                confirm_quit(w, &notebook, &quitting); // no tray -> confirm exit
            }
            glib::Propagation::Stop
        });
    }

    // Stash the quit path on the window so menu actions can reach it.
    {
        let window = window.clone();
        let notebook = notebook.clone();
        let quitting = quitting.clone();
        let quit = gio::SimpleAction::new("quit", None);
        quit.connect_activate({
            let window = window.clone();
            move |_, _| confirm_quit(&window, &notebook, &quitting)
        });
        window.add_action(&quit);
    }

    // Tray: keep the Handle alive for the app's lifetime by leaking it.
    if let Some((rx, handle)) = tray {
        Box::leak(Box::new(handle));
        let window = window.clone();
        let notebook = notebook.clone();
        let quitting = quitting.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    tray::TrayMsg::Show => {
                        window.set_visible(true);
                        window.present();
                    }
                    tray::TrayMsg::Quit => confirm_quit(&window, &notebook, &quitting),
                }
            }
        });
    }
}

/// Ask before really quitting; warns if any tab still has a live SSH session.
fn confirm_quit(window: &ApplicationWindow, notebook: &gtk::Notebook, quitting: &Rc<std::cell::Cell<bool>>) {
    let active = (0..notebook.n_pages())
        .filter_map(|i| notebook.nth_page(Some(i)))
        .filter(tab_is_connected)
        .count();

    let msg = if active > 0 {
        format!("{active} connection(s) still open. Quit anyway?")
    } else {
        "Quit Simple SSH Manager?".to_string()
    };

    let dialog = gtk::Window::builder()
        .title("Quit")
        .transient_for(window)
        .modal(true)
        .destroy_with_parent(true)
        .build();
    let label = gtk::Label::builder()
        .label(msg)
        .margin_top(16).margin_bottom(8).margin_start(16).margin_end(16)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Quit");
    confirm.add_css_class("destructive-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6).halign(gtk::Align::End)
        .margin_bottom(12).margin_end(12).build();
    buttons.append(&cancel);
    buttons.append(&confirm);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&label);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });
    confirm.connect_clicked({
        let dialog = dialog.clone();
        let window = window.clone();
        let quitting = quitting.clone();
        move |_| {
            quitting.set(true);
            dialog.close();
            window.close(); // close-request now proceeds -> app exits
        }
    });
    dialog.present();
}

/// Build the menu bar: Options (Add / Change Master Password / Exit) and Help (About).
fn build_menubar() -> gtk::PopoverMenuBar {
    let options = gio::Menu::new();
    options.append(Some("Add Server"), Some("win.add-server"));
    options.append(Some("Change Master Password"), Some("win.change-master"));
    options.append(Some("Exit"), Some("win.exit"));

    let help = gio::Menu::new();
    help.append(Some("About"), Some("win.about"));

    let menu = gio::Menu::new();
    menu.append_submenu(Some("Options"), &options);
    menu.append_submenu(Some("Help"), &help);
    gtk::PopoverMenuBar::from_model(Some(&menu))
}

/// Wire the win.* actions referenced by the menu bar.
fn install_menu_actions(window: &ApplicationWindow, state: &State, list: &gtk::ListBox) {
    let add = gio::SimpleAction::new("add-server", None);
    add.connect_activate({
        let window = window.clone();
        let state = state.clone();
        let list = list.clone();
        move |_, _| show_edit_dialog(&window, state.clone(), list.clone(), None)
    });
    window.add_action(&add);

    let change = gio::SimpleAction::new("change-master", None);
    change.connect_activate({
        let window = window.clone();
        let state = state.clone();
        move |_, _| show_change_master_dialog(&window, state.clone())
    });
    window.add_action(&change);

    let exit = gio::SimpleAction::new("exit", None);
    exit.connect_activate({
        let window = window.clone();
        move |_, _| { let _ = gtk::prelude::WidgetExt::activate_action(&window, "win.quit", None); }
    });
    window.add_action(&exit);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate({
        let window = window.clone();
        move |_, _| show_about_dialog(&window)
    });
    window.add_action(&about);
}

/// Help -> About: short description on the main page; email/repo under Credits.
fn show_about_dialog(parent: &ApplicationWindow) {
    let about = gtk::AboutDialog::builder()
        .transient_for(parent)
        .modal(true)
        .program_name("Simple SSH Manager")
        .logo_icon_name(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A simple SSH connection manager with an encrypted server store, grouped hosts, and an embedded terminal.")
        .authors(vec!["Mohamed Fawzy".to_string()])
        .website("https://sadeem.cloud")
        .website_label("https://sadeem.cloud")
        .build();
    // Email and repo live in the Credits tab to keep the main page short.
    about.add_credit_section("Email", &["mf.elsaigh@gmail.com"]);
    about.add_credit_section("Repository", &["https://github.com/mf-elsaigh/simple-ssh-manager.git"]);
    center::center_on_monitor(&about);
    about.present();
}

/// Change the master password: re-encrypt the store under a new passphrase.
fn show_change_master_dialog(parent: &ApplicationWindow, state: State) {
    let dialog = gtk::Window::builder()
        .title("Change Master Password")
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .build();

    let new1 = gtk::PasswordEntry::builder().show_peek_icon(true).build();
    let new2 = gtk::PasswordEntry::builder().show_peek_icon(true).build();
    let status = gtk::Label::new(None);
    status.add_css_class("error");

    let grid = gtk::Grid::builder()
        .row_spacing(6).column_spacing(6)
        .margin_top(12).margin_bottom(8).margin_start(12).margin_end(12).build();
    grid.attach(&gtk::Label::builder().label("New password").xalign(0.0).build(), 0, 0, 1, 1);
    grid.attach(&new1, 1, 0, 1, 1);
    grid.attach(&gtk::Label::builder().label("Confirm").xalign(0.0).build(), 0, 1, 1, 1);
    grid.attach(&new2, 1, 1, 1, 1);

    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Change");
    ok.add_css_class("suggested-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6).halign(gtk::Align::End)
        .margin_bottom(12).margin_end(12).build();
    buttons.append(&cancel);
    buttons.append(&ok);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&grid);
    content.append(&status);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });
    ok.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            let p1 = new1.text().to_string();
            let p2 = new2.text().to_string();
            if p1.is_empty() {
                status.set_text("Password cannot be empty.");
                return;
            }
            if p1 != p2 {
                status.set_text("Passwords do not match.");
                return;
            }
            let secret = SecretString::from(p1);
            let mut st = state.borrow_mut();
            st.password = secret;
            save_servers(&st); // re-encrypts under the new password
            dialog.close();
        }
    });
    dialog.present();
}

/// Sidebar rebuild context: wires a right-click Edit/Delete menu onto each server row.
#[derive(Clone)]
struct RowCtx {
    state: State,
    window: ApplicationWindow,
    list: gtk::ListBox,
}

fn rebuild_list_ctx(list: &gtk::ListBox, servers: &[Server], ctx: Option<RowCtx>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    // Group paths in first-appearance order, e.g. "catg1/subcatg1".
    let mut groups: Vec<String> = Vec::new();
    for s in servers {
        let g = group_name(s);
        if !groups.contains(&g) {
            groups.push(g);
        }
    }
    // Track which header path segments are already drawn, so a shared parent
    // (e.g. "catg1") is shown once above its subgroups.
    let mut drawn: Vec<String> = Vec::new();
    for g in &groups {
        for (label, depth) in headers_to_draw(g, &mut drawn) {
            append_header(list, &label, depth);
        }
        // Servers sit one level deeper than their deepest group header.
        let server_depth = g.split('/').count();
        for (idx, s) in servers.iter().enumerate() {
            if group_name(s) == *g {
                let row = append_server_row(list, idx, &s.name, server_depth);
                if let Some(ctx) = &ctx {
                    attach_row_menu(&row, idx, ctx.clone());
                }
            }
        }
    }
}

/// Headers (label, depth) that need drawing for group path `g`, given the set of
/// already-drawn paths. Pushes newly drawn paths into `drawn`.
fn headers_to_draw(g: &str, drawn: &mut Vec<String>) -> Vec<(String, usize)> {
    let parts: Vec<&str> = g.split('/').collect();
    let mut out = Vec::new();
    for depth in 0..parts.len() {
        let path = parts[..=depth].join("/");
        if !drawn.contains(&path) {
            out.push((parts[depth].to_string(), depth));
            drawn.push(path);
        }
    }
    out
}

#[cfg(test)]
mod group_tests {
    use super::headers_to_draw;

    #[test]
    fn nested_parent_drawn_once() {
        let mut drawn = Vec::new();
        // First subgroup draws both levels.
        assert_eq!(
            headers_to_draw("catg1/subcatg1", &mut drawn),
            vec![("catg1".to_string(), 0), ("subcatg1".to_string(), 1)]
        );
        // Sibling subgroup reuses the parent header.
        assert_eq!(
            headers_to_draw("catg1/subcatg2", &mut drawn),
            vec![("subcatg2".to_string(), 1)]
        );
        // Unrelated top-level group.
        assert_eq!(
            headers_to_draw("other", &mut drawn),
            vec![("other".to_string(), 0)]
        );
    }
}

/// Right-click on a server row -> themed PopoverMenu with Edit / Delete.
fn attach_row_menu(row: &gtk::ListBoxRow, idx: usize, ctx: RowCtx) {
    // Per-row action group so each row's actions target its own index.
    let actions = gio::SimpleActionGroup::new();
    let edit = gio::SimpleAction::new("edit", None);
    edit.connect_activate({
        let ctx = ctx.clone();
        move |_, _| show_edit_dialog(&ctx.window, ctx.state.clone(), ctx.list.clone(), Some(idx))
    });
    let delete = gio::SimpleAction::new("delete", None);
    delete.connect_activate({
        let ctx = ctx.clone();
        move |_, _| confirm_delete(&ctx.window, ctx.state.clone(), ctx.list.clone(), idx)
    });
    actions.add_action(&edit);
    actions.add_action(&delete);
    row.insert_action_group("row", Some(&actions));

    let model = gio::Menu::new();
    model.append(Some("Edit"), Some("row.edit"));
    model.append(Some("Delete"), Some("row.delete"));

    let popover = gtk::PopoverMenu::from_model(Some(&model));
    popover.set_has_arrow(false);
    popover.set_parent(row);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |_, _, x, y| {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    row.add_controller(gesture);
}

/// Confirm then delete server `idx`.
fn confirm_delete(parent: &ApplicationWindow, state: State, list: gtk::ListBox, idx: usize) {
    let name = state.borrow().servers.get(idx).map(|s| s.name.clone()).unwrap_or_default();
    let dialog = gtk::Window::builder()
        .title("Delete Server")
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .build();
    let label = gtk::Label::builder()
        .label(format!("Delete \"{name}\"?"))
        .margin_top(16).margin_bottom(8).margin_start(16).margin_end(16)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Delete");
    confirm.add_css_class("destructive-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6).halign(gtk::Align::End)
        .margin_bottom(12).margin_end(12).build();
    buttons.append(&cancel);
    buttons.append(&confirm);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&label);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });
    confirm.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            {
                let mut st = state.borrow_mut();
                if idx < st.servers.len() {
                    st.servers.remove(idx);
                    save_servers(&st);
                }
            }
            refresh_sidebar(&list, &state, &dialog_parent(&dialog));
            dialog.close();
        }
    });
    dialog.present();
}

/// Helper: the ApplicationWindow a transient dialog belongs to.
fn dialog_parent(dialog: &gtk::Window) -> ApplicationWindow {
    dialog.transient_for()
        .and_then(|w| w.downcast::<ApplicationWindow>().ok())
        .expect("dialog has an ApplicationWindow parent")
}

/// Rebuild the sidebar with context menus intact.
fn refresh_sidebar(list: &gtk::ListBox, state: &State, window: &ApplicationWindow) {
    let servers = state.borrow().servers.clone();
    rebuild_list_ctx(
        list,
        &servers,
        Some(RowCtx { state: state.clone(), window: window.clone(), list: list.clone() }),
    );
}

/// Normalized group path: trims each "a / b" segment; empty => "Ungrouped".
fn group_name(s: &Server) -> String {
    let parts: Vec<&str> = s.group.split('/').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        "Ungrouped".to_string()
    } else {
        parts.join("/")
    }
}

/// Indent in pixels per nesting level.
const INDENT: i32 = 14;

fn append_header(list: &gtk::ListBox, name: &str, depth: usize) {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    let label = gtk::Label::builder()
        .label(name).xalign(0.0)
        .margin_top(8).margin_bottom(2)
        .margin_start(6 + depth as i32 * INDENT).margin_end(6)
        .build();
    label.add_css_class("heading");
    label.add_css_class("dim-label");
    row.set_child(Some(&label));
    list.append(&row);
}

fn append_server_row(list: &gtk::ListBox, server_index: usize, name: &str, depth: usize) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    // Tag the row with its index into the servers Vec.
    unsafe { row.set_data("server-index", server_index) };
    let label = gtk::Label::builder()
        .label(name).xalign(0.0)
        .margin_top(6).margin_bottom(6)
        .margin_start(6 + depth as i32 * INDENT).margin_end(8)
        .build();
    row.set_child(Some(&label));
    list.append(&row);
    row
}

fn row_server_index(row: &gtk::ListBoxRow) -> Option<usize> {
    unsafe { row.data::<usize>("server-index").map(|p| *p.as_ref()) }
}

/// Persist the current server list; log on failure rather than silently dropping data.
fn save_servers(st: &AppState) {
    if let Err(e) = store::save(&st.servers, &st.password) {
        eprintln!("save failed: {e}");
    }
}

/// Add (`edit = None`) or edit (`edit = Some(index)`) a server.
fn show_edit_dialog(
    parent: &ApplicationWindow,
    state: State,
    list: gtk::ListBox,
    edit: Option<usize>,
) {
    let existing = edit.and_then(|i| state.borrow().servers.get(i).cloned());
    let dialog = gtk::Window::builder()
        .title(if edit.is_some() { "Edit Server" } else { "Add Server" })
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .build();

    let pre = |f: fn(&Server) -> &str| existing.as_ref().map(|s| f(s)).unwrap_or("");
    let name = gtk::Entry::builder().placeholder_text("Name").text(pre(|s| &s.name)).build();
    let host = gtk::Entry::builder().placeholder_text("Host").text(pre(|s| &s.host)).build();
    let user = gtk::Entry::builder().placeholder_text("User").text(pre(|s| &s.user)).build();
    let group = gtk::Entry::builder().placeholder_text("Group (optional)").text(pre(|s| &s.group)).build();
    let password = gtk::Entry::builder()
        .placeholder_text("Password (blank = key or agent)")
        .text(pre(|s| &s.password))
        .visibility(false).build();

    // Key file picker: an entry + Browse button.
    let keyfile = gtk::Entry::builder()
        .placeholder_text("Private key file (optional)")
        .text(pre(|s| &s.keyfile))
        .hexpand(true).build();
    let browse = gtk::Button::with_label("Browse");
    let key_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    key_row.append(&keyfile);
    key_row.append(&browse);

    {
        let keyfile = keyfile.clone();
        let dialog = dialog.clone();
        browse.connect_clicked(move |_| {
            let file_dialog = gtk::FileDialog::builder().title("Select Private Key").build();
            let keyfile = keyfile.clone();
            file_dialog.open(Some(&dialog), gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        keyfile.set_text(&path.to_string_lossy());
                    }
                }
            });
        });
    }

    let grid = gtk::Grid::builder()
        .row_spacing(6).column_spacing(6).margin_top(12)
        .margin_bottom(12).margin_start(12).margin_end(12).build();
    let mut r = 0;
    let mut field = |label: &str, w: &gtk::Widget| {
        grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, r, 1, 1);
        grid.attach(w, 1, r, 1, 1);
        r += 1;
    };
    field("Name", name.upcast_ref());
    field("Host", host.upcast_ref());
    field("User", user.upcast_ref());
    field("Group", group.upcast_ref());
    field("Password", password.upcast_ref());
    field("Key file", key_row.upcast_ref());

    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6).halign(gtk::Align::End)
        .margin_bottom(12).margin_end(12).build();
    buttons.append(&cancel);
    buttons.append(&ok);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&grid);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });
    ok.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            let s = Server {
                name: name.text().to_string(),
                host: host.text().to_string(),
                user: user.text().to_string(),
                group: group.text().to_string(),
                password: password.text().to_string(),
                keyfile: keyfile.text().to_string(),
            };
            if !s.name.is_empty() && !s.host.is_empty() {
                {
                    let mut st = state.borrow_mut();
                    match edit {
                        Some(i) if i < st.servers.len() => st.servers[i] = s,
                        _ => st.servers.push(s),
                    }
                    save_servers(&st);
                }
                refresh_sidebar(&list, &state, &dialog_parent(&dialog));
            }
            dialog.close();
        }
    });

    dialog.present();
}

fn open_terminal_tab(notebook: &gtk::Notebook, server: &Server) {
    let terminal = Terminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);

    // Copy/paste actions (VTE binds neither by default), shared by the keyboard
    // shortcuts and the right-click menu.
    let actions = gio::SimpleActionGroup::new();
    let copy = gio::SimpleAction::new("copy", None);
    copy.connect_activate({
        let terminal = terminal.clone();
        move |_, _| terminal.copy_clipboard_format(vte4::Format::Text)
    });
    let paste = gio::SimpleAction::new("paste", None);
    paste.connect_activate({
        let terminal = terminal.clone();
        move |_, _| terminal.paste_clipboard()
    });
    actions.add_action(&copy);
    actions.add_action(&paste);
    terminal.insert_action_group("term", Some(&actions));

    // Ctrl+Shift+C / Ctrl+Shift+V -> the actions above.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl_shift = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
        if mods.contains(ctrl_shift) {
            match key {
                gtk::gdk::Key::C => { copy.activate(None); return glib::Propagation::Stop; }
                gtk::gdk::Key::V => { paste.activate(None); return glib::Propagation::Stop; }
                _ => {}
            }
        }
        glib::Propagation::Proceed
    });
    terminal.add_controller(keys);

    // Right-click menu with Copy/Paste, shortcuts shown beside each label.
    let model = gio::Menu::new();
    let copy_item = gio::MenuItem::new(Some("Copy"), Some("term.copy"));
    copy_item.set_attribute_value("accel", Some(&"<Ctrl><Shift>c".to_variant()));
    let paste_item = gio::MenuItem::new(Some("Paste"), Some("term.paste"));
    paste_item.set_attribute_value("accel", Some(&"<Ctrl><Shift>v".to_variant()));
    model.append_item(&copy_item);
    model.append_item(&paste_item);

    let popover = gtk::PopoverMenu::from_model(Some(&model));
    popover.set_has_arrow(false);
    popover.set_parent(&terminal);
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |_, _, x, y| {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    terminal.add_controller(gesture);

    let target = format!("{}@{}", server.user, server.host);

    // Build argv:
    //  - key file  -> ssh -i <key> user@host
    //  - password  -> sshpass -e ssh user@host   (password via SSHPASS env)
    //  - neither   -> ssh user@host              (agent / default keys)
    let mut argv: Vec<String> = Vec::new();
    let mut env: Vec<String> = Vec::new();
    if !server.password.is_empty() {
        argv.push("sshpass".into());
        argv.push("-e".into());
        env.push(format!("SSHPASS={}", server.password));
    }
    argv.push("ssh".into());
    if !server.keyfile.trim().is_empty() {
        argv.push("-i".into());
        argv.push(server.keyfile.trim().to_string());
    }
    argv.push(target);

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_refs: Vec<&str> = env.iter().map(String::as_str).collect();
    terminal.spawn_async(
        PtyFlags::DEFAULT,
        None,
        &argv_refs,
        &env_refs,
        glib::SpawnFlags::DEFAULT,
        || {},
        -1,
        None::<&gio::Cancellable>,
        {
            // Mark the tab connected once the child spawns; cleared on exit below.
            let terminal = terminal.clone();
            move |res| {
                if res.is_ok() {
                    unsafe { terminal.set_data("connected", true) };
                }
            }
        },
    );
    // Child exited (logout, dropped connection) -> tab is no longer "connected",
    // so closing it won't prompt.
    terminal.connect_child_exited({
        let terminal = terminal.clone();
        move |_, _| unsafe { terminal.set_data("connected", false) }
    });

    let tab_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let title = gtk::Label::new(Some(&server.name));
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.set_has_frame(false);
    tab_box.append(&title);
    tab_box.append(&close);

    notebook.append_page(&terminal, Some(&tab_box));
    notebook.set_current_page(notebook.page_num(&terminal));

    {
        let notebook = notebook.clone();
        let terminal = terminal.clone();
        let name = server.name.clone();
        close.connect_clicked(move |_| {
            let close_page = {
                let notebook = notebook.clone();
                let terminal = terminal.clone();
                move || {
                    if let Some(n) = notebook.page_num(&terminal) {
                        notebook.remove_page(Some(n));
                    }
                }
            };
            // Only prompt while the SSH session is live.
            if tab_is_connected(terminal.upcast_ref()) {
                if let Some(window) = notebook.root().and_downcast::<gtk::Window>() {
                    confirm_close_tab(&window, &name, close_page);
                    return;
                }
            }
            close_page();
        });
    }
}

/// True if the terminal's child process is still running (set on spawn, cleared
/// on child-exited).
fn tab_is_connected(page: &gtk::Widget) -> bool {
    unsafe { page.data::<bool>("connected").map(|p| *p.as_ref()).unwrap_or(false) }
}

/// Confirm closing a tab whose SSH session is still connected.
fn confirm_close_tab(window: &gtk::Window, name: &str, on_confirm: impl Fn() + 'static) {
    let dialog = gtk::Window::builder()
        .title("Close Connection")
        .transient_for(window)
        .modal(true)
        .destroy_with_parent(true)
        .build();
    let label = gtk::Label::builder()
        .label(format!("\"{name}\" is still connected. Close this tab?"))
        .margin_top(16).margin_bottom(8).margin_start(16).margin_end(16)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Close");
    confirm.add_css_class("destructive-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6).halign(gtk::Align::End)
        .margin_bottom(12).margin_end(12).build();
    buttons.append(&cancel);
    buttons.append(&confirm);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&label);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });
    confirm.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            on_confirm();
            dialog.close();
        }
    });
    dialog.present();
}
