//! System tray icon via StatusNotifierItem (ksni). GTK4 has no native tray, and
//! ksni runs on its own thread, so its menu callbacks can't touch GTK widgets.
//! Instead they push a `TrayMsg` onto an async-channel that the GTK main context
//! drains on its own thread.

use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Tray};

#[derive(Debug, Clone, Copy)]
pub enum TrayMsg {
    Show,
    Quit,
}

struct AppTray {
    icon_name: String,
    tx: async_channel::Sender<TrayMsg>,
}

impl Tray for AppTray {
    fn id(&self) -> String {
        "cloud.sadeem.SimpleSshManager".into()
    }
    fn title(&self) -> String {
        "Simple SSH Manager".into()
    }
    // Prefer the themed icon by name; trays that ignore names fall back to title.
    fn icon_name(&self) -> String {
        self.icon_name.clone()
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        Vec::new()
    }
    // Left-click the tray icon -> show the window.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(TrayMsg::Show);
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show".into(),
                activate: Box::new(|t: &mut AppTray| {
                    let _ = t.tx.try_send(TrayMsg::Show);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut AppTray| {
                    let _ = t.tx.try_send(TrayMsg::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Spawn the tray and return the receiver for tray events. The returned `Handle`
/// must be kept alive or the tray vanishes. Returns None if the tray host is
/// unavailable (no SNI watcher), so the app still runs without a tray.
pub fn spawn(icon_name: &str) -> Option<(async_channel::Receiver<TrayMsg>, ksni::blocking::Handle<impl Tray>)> {
    use ksni::blocking::TrayMethods;
    let (tx, rx) = async_channel::unbounded();
    let tray = AppTray { icon_name: icon_name.to_string(), tx };
    match tray.spawn() {
        Ok(handle) => Some((rx, handle)),
        Err(e) => {
            eprintln!("tray unavailable: {e}");
            None
        }
    }
}
