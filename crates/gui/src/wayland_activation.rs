//! Raising the window on Wayland. Compositors keep windows from taking the
//! focus on their own; a launcher's activation token (xdg-activation), handed
//! over by a second launch, lets the running window come to the front.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use sctk::activation::{ActivationHandler, ActivationState, RequestData};
use sctk::reexports::client::backend::{Backend, ObjectId};
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::{delegate_activation, delegate_registry, registry_handlers};
use smithay_client_toolkit as sctk;

/// The window's Wayland display and surface.
pub struct Activator {
    display: usize,
    surface: usize,
}

impl Activator {
    pub fn new(handle: &(impl HasDisplayHandle + HasWindowHandle)) -> Option<Self> {
        let RawDisplayHandle::Wayland(display) = handle.display_handle().ok()?.as_raw() else { return None };
        let RawWindowHandle::Wayland(window) = handle.window_handle().ok()?.as_raw() else { return None };
        Some(Activator { display: display.display.as_ptr() as usize, surface: window.surface.as_ptr() as usize })
    }

    /// Asks the compositor to bring the window to the front with `token`.
    pub fn activate(&self, token: &str) -> Option<()> {
        // Safety: the display and the surface live as long as the window, which
        // lives as long as the app.
        let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(self.display as *mut _) });
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let activation = ActivationState::bind(&globals, &qh).ok()?;
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), self.surface as *mut _) }.ok()?;
        let surface = WlSurface::from_id(&conn, id).ok()?;
        activation.activate::<State>(&surface, token.to_string());
        queue.roundtrip(&mut State { registry: RegistryState::new(&globals) }).ok()?;
        Some(())
    }
}

struct State {
    registry: RegistryState,
}

impl ActivationHandler for State {
    type RequestData = RequestData;
    fn new_token(&mut self, _: String, _: &RequestData) {}
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![];
}

delegate_registry!(State);
delegate_activation!(State);
