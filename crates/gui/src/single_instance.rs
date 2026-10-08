//! One app at a time. The first launch owns the app's name on the D-Bus
//! session bus and serves `org.freedesktop.Application`; a later launch hands
//! it the files to open (and the launcher's activation token, to raise its
//! window) and exits. Flatpak lets an app own the name of its own id.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;
use zbus::zvariant::{OwnedValue, Value};

use crate::APP_ID;

const PATH: &str = "/io/github/the_beper/PaletoModkit";
const INTERFACE: &str = "org.freedesktop.Application";

/// What a later launch asks of the running app.
pub struct Request {
    pub files: Vec<PathBuf>,
    /// The launcher's xdg-activation token, to raise the window.
    pub token: Option<String>,
}

pub enum Claim {
    /// This is the only instance: requests from later launches come here.
    First(Instance),
    /// Another instance took the files over: this one should exit.
    HandedOver,
    /// No session bus: run without the check.
    Unavailable,
}

pub struct Instance {
    requests: Receiver<Request>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    /// Keeps the name and the served object.
    _conn: zbus::Connection,
}

impl Instance {
    /// Repaints `ctx` when a request comes, so it is seen at once.
    pub fn attach(&self, ctx: &egui::Context) {
        *self.ctx.lock().unwrap() = Some(ctx.clone());
    }

    pub fn poll(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }
}

struct Service {
    tx: Mutex<Sender<Request>>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl Service {
    fn send(&self, files: Vec<PathBuf>, platform_data: &HashMap<String, OwnedValue>) {
        let token = ["activation-token", "desktop-startup-id"]
            .iter()
            .find_map(|k| platform_data.get(*k).and_then(|v| String::try_from(v.try_clone().ok()?).ok()));
        let _ = self.tx.lock().unwrap().send(Request { files, token });
        if let Some(ctx) = self.ctx.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }
}

#[zbus::interface(name = "org.freedesktop.Application")]
impl Service {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        self.send(Vec::new(), &platform_data);
    }

    fn open(&self, uris: Vec<String>, platform_data: HashMap<String, OwnedValue>) {
        let files = uris.iter().flat_map(|u| crate::wayland_dnd::parse_uri_list(u)).collect();
        self.send(files, &platform_data);
    }

    fn activate_action(&self, _name: String, _parameter: Vec<OwnedValue>, platform_data: HashMap<String, OwnedValue>) {
        self.send(Vec::new(), &platform_data);
    }
}

/// Becomes the app's only instance, or hands `files` to the running one.
pub fn claim(files: &[PathBuf]) -> Claim {
    claim_name(APP_ID, files)
}

fn claim_name(name: &str, files: &[PathBuf]) -> Claim {
    zbus::block_on(async {
        let Ok(conn) = zbus::Connection::session().await else { return Claim::Unavailable };
        let (tx, requests) = channel();
        let ctx = Arc::new(Mutex::new(None));
        let service = Service { tx: Mutex::new(tx), ctx: ctx.clone() };
        if conn.object_server().at(PATH, service).await.is_err() {
            return Claim::Unavailable;
        }
        let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
        match conn.request_name_with_flags(name, flags).await {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner) => {
                Claim::First(Instance { requests, ctx, _conn: conn })
            }
            Ok(_) | Err(zbus::Error::NameTaken) => match hand_over(&conn, name, files).await {
                Ok(()) => Claim::HandedOver,
                Err(_) => Claim::Unavailable,
            },
            Err(_) => Claim::Unavailable,
        }
    })
}

async fn hand_over(conn: &zbus::Connection, name: &str, files: &[PathBuf]) -> zbus::Result<()> {
    // The launcher's token: Wayland's, or the older startup notification id.
    let token = std::env::var("XDG_ACTIVATION_TOKEN").or_else(|_| std::env::var("DESKTOP_STARTUP_ID")).ok();
    let mut platform_data: HashMap<&str, Value> = HashMap::new();
    if let Some(token) = &token {
        platform_data.insert("activation-token", Value::from(token.as_str()));
        platform_data.insert("desktop-startup-id", Value::from(token.as_str()));
    }
    if files.is_empty() {
        conn.call_method(Some(name), PATH, Some(INTERFACE), "Activate", &(platform_data,)).await?;
    } else {
        let uris: Vec<String> = files.iter().map(|f| file_uri(f)).collect();
        conn.call_method(Some(name), PATH, Some(INTERFACE), "Open", &(uris, platform_data)).await?;
    }
    Ok(())
}

/// `file://` URI of a path, made absolute (the running app has another
/// working directory), with the bytes URIs cannot hold percent-encoded.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let path = PathBuf::from("/home/me/My Mods/città #1.oiv");
        let uri = file_uri(&path);
        assert_eq!(uri, "file:///home/me/My%20Mods/citt%C3%A0%20%231.oiv");
        assert_eq!(crate::wayland_dnd::parse_uri_list(&uri), [path]);
    }

    /// A second launch hands its files over (under a name of its own, so a
    /// running app is not disturbed).
    #[test]
    fn second_launch_hands_over() {
        let name = format!("{APP_ID}.Test{}", std::process::id());
        // Without a session bus (as in CI) there is nothing to test.
        let Claim::First(first) = claim_name(&name, &[]) else { return };
        let file = PathBuf::from("/tmp/some mod.oiv");
        assert!(matches!(claim_name(&name, std::slice::from_ref(&file)), Claim::HandedOver));
        let request = (0..200).find_map(|_| first.poll().or_else(|| { std::thread::sleep(std::time::Duration::from_millis(10)); None })).unwrap();
        assert_eq!(request.files, [file]);
        // A launch without files only raises the window.
        assert!(matches!(claim_name(&name, &[]), Claim::HandedOver));
        let request = (0..200).find_map(|_| first.poll().or_else(|| { std::thread::sleep(std::time::Duration::from_millis(10)); None })).unwrap();
        assert!(request.files.is_empty());
    }
}
