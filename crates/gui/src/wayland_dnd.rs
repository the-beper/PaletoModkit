//! Files dropped on the window under Wayland.
//!
//! winit 0.30 does not implement drag and drop on Wayland, so this listens on
//! the window's own Wayland connection with a separate event queue (as egui's
//! clipboard does) and reports `text/uri-list` drops. Inside a sandbox, files
//! come through the document portal instead (`application/vnd.portal.filetransfer`).

use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::reexports::client::backend::Backend;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, QueueHandle};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{delegate_data_device, delegate_registry, delegate_seat, registry_handlers};
use smithay_client_toolkit as sctk;

const URI_LIST: &str = "text/uri-list";
/// A key to pass to the portal's FileTransfer.RetrieveFiles.
const PORTAL_FILES: &str = "application/vnd.portal.filetransfer";

pub enum DndEvent {
    /// Files are being dragged over the window (true) or left it (false).
    Hover(bool),
    Dropped(Vec<PathBuf>),
    /// Something was dropped but its files could not be had.
    Failed(String),
}

/// Starts listening for drops when the window is on Wayland.
pub fn listen(handle: &impl HasDisplayHandle, ctx: &egui::Context) -> Option<Receiver<DndEvent>> {
    let RawDisplayHandle::Wayland(display) = handle.display_handle().ok()?.as_raw() else { return None };
    // Safety: the display stays alive as long as the window, which lives as long as the app.
    let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) });
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::Builder::new().name("wayland-dnd".into()).spawn(move || run(conn, tx, ctx)).ok()?;
    Some(rx)
}

fn run(conn: Connection, tx: Sender<DndEvent>, ctx: egui::Context) {
    let Ok((globals, mut queue)) = registry_queue_init::<State>(&conn) else { return };
    let qh = queue.handle();
    let Ok(manager) = DataDeviceManagerState::bind(&globals, &qh) else { return };
    let mut state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        manager,
        devices: Vec::new(),
        conn: conn.clone(),
        tx,
        ctx,
    };
    for seat in state.seats.seats().collect::<Vec<_>>() {
        state.devices.push(state.manager.get_data_device(&qh, &seat));
    }
    while queue.blocking_dispatch(&mut state).is_ok() {}
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    manager: DataDeviceManagerState,
    devices: Vec<DataDevice>,
    conn: Connection,
    tx: Sender<DndEvent>,
    ctx: egui::Context,
}

impl State {
    fn send(&self, event: DndEvent) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }

    fn offer(&self, device: &WlDataDevice) -> Option<DragOffer> {
        self.devices.iter().find(|d| d.inner() == device)?.data().drag_offer()
    }
}

/// The type to read from an offer: the portal's when sandboxed, as host paths
/// in a uri-list may not be visible there.
fn mime_type(offer: &DragOffer) -> Option<&'static str> {
    let sandboxed = crate::sandboxed();
    offer.with_mime_types(|types| {
        let has = |mime: &str| types.iter().any(|t| t == mime);
        if sandboxed && has(PORTAL_FILES) {
            Some(PORTAL_FILES)
        } else {
            has(URI_LIST).then_some(URI_LIST)
        }
    })
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice, _: f64, _: f64, _: &WlSurface) {
        let Some(offer) = self.offer(device) else { return };
        if let Some(mime) = mime_type(&offer) {
            offer.accept_mime_type(offer.serial, Some(mime.into()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
            self.send(DndEvent::Hover(true));
        } else {
            offer.accept_mime_type(offer.serial, None);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.send(DndEvent::Hover(false));
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = self.offer(device) else { return };
        self.send(DndEvent::Hover(false));
        let Some(mime) = mime_type(&offer) else { return };
        let has_uri_list = offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST));
        // The source writes into the pipe only once the request reaches it, and
        // reading must not block this queue: read on another thread.
        let (conn, tx, ctx) = (self.conn.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let read = |mime: &str| -> Option<String> {
                let mut pipe = offer.receive(mime.into()).ok()?;
                let _ = conn.flush();
                let mut text = String::new();
                pipe.read_to_string(&mut text).ok()?;
                Some(text)
            };
            // Through the portal, the files must be retrieved before the drop
            // is finished: the source then ends the transfer.
            let mut result = match mime {
                PORTAL_FILES => read(PORTAL_FILES).ok_or_else(|| "the dropped data could not be read".to_string()).and_then(|key| retrieve_files(&key)),
                _ => Ok(Vec::new()),
            };
            if result.as_ref().is_ok_and(Vec::is_empty) && has_uri_list {
                let fallback = read(URI_LIST).map(|list| parse_uri_list(&list)).unwrap_or_default();
                result = match result {
                    Err(e) if fallback.is_empty() => Err(e),
                    _ => Ok(fallback),
                };
            }
            offer.finish();
            offer.destroy();
            let _ = conn.flush();
            let event = match result {
                Ok(paths) if paths.is_empty() => DndEvent::Failed("no local files in what was dropped".into()),
                Ok(paths) => DndEvent::Dropped(paths),
                Err(e) => DndEvent::Failed(e),
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, _: DndAction) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

/// Only needed by `delegate_data_device!`: this client never offers data.
impl DataSourceHandler for State {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        self.devices.push(self.manager.get_data_device(qh, &seat));
    }

    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.devices.retain(|d| d.data().seat() != &seat);
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![SeatState];
}

delegate_registry!(State);
delegate_seat!(State);
delegate_data_device!(State);

/// The files behind a portal transfer key, as paths visible in the sandbox.
fn retrieve_files(key: &str) -> Result<Vec<PathBuf>, String> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let key = key.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    let files: zbus::Result<Vec<String>> = zbus::block_on(async {
        let conn = zbus::Connection::session().await?;
        let options: HashMap<&str, Value> = HashMap::new();
        let reply = conn
            .call_method(
                Some("org.freedesktop.portal.Documents"),
                "/org/freedesktop/portal/documents",
                Some("org.freedesktop.portal.FileTransfer"),
                "RetrieveFiles",
                &(key, options),
            )
            .await?;
        reply.body().deserialize()
    });
    files.map(|f| f.into_iter().map(PathBuf::from).collect()).map_err(|e| format!("the file transfer portal refused the drop ({e})"))
}

/// Local paths in a `text/uri-list` (RFC 2483): one URI per line, `#` comments.
pub fn parse_uri_list(list: &str) -> Vec<PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|uri| uri.strip_prefix("file://"))
        // An optional host ("localhost") precedes the absolute path.
        .filter_map(|rest| rest.find('/').map(|i| &rest[i..]))
        .map(|path| PathBuf::from(percent_decode(path)))
        .collect()
}

fn percent_decode(s: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    std::ffi::OsString::from_vec(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists() {
        let list = "# from Dolphin\r\nfile:///home/me/My%20Mods/car.oiv\r\nfile://localhost/tmp/a%25b.dds\r\nhttps://example.com/x\r\n";
        assert_eq!(parse_uri_list(list), [PathBuf::from("/home/me/My Mods/car.oiv"), PathBuf::from("/tmp/a%b.dds")]);
    }
}
