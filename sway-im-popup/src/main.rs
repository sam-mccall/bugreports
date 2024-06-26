// This demo input method (input-method-unstable-v2) shows an apparent bug in
// sway's handling of the popup surface (i.e. candidate selection window).
//
// It has the usual structure: grabs keyboard when a text input is activated,
// and forwards keys to the application via virtual keyboard.
//
// To demonstrate the popup behavior:
//  - it is toggled with each key: first key shows it, second hides it, etc
//  - it is initially blue and fills up vertically with red (1px per 10 frames)
//
// Buggy behavior under current sway (5a7477cb8f568ce4aeb852215ad40899f18f3d91):
//  - after the first keystroke we correctly see the panel
//  - after the second keystroke we correctly see nothing
//  - after the third keystroke we see the panel *twice*, once in the correct
//    (new) position and once in the original position. Both are animated.
//  - after the fourth keystroke, both panels disappear
//  - after the fifth keystroke, we have three panels, etc
//  - the more panels we have, the faster the animation: the surface is getting
//    multiple redraw calls per frame
//  - when we deactivate the text area, sway crashes (if we showed >=2 panels)

use std::{error::Error, os::fd::AsFd, sync::atomic::AtomicUsize};

use protocol::{
    wl_buffer::WlBuffer,
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_shm::{self, WlShm},
    wl_surface::WlSurface,
};

use smithay_client_toolkit::{
    delegate_registry,
    globals::ProvidesBoundGlobal,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shm::slot::{Buffer, SlotPool},
};
use wayland_client::{
    delegate_noop,
    globals::registry_queue_init,
    protocol::{self, wl_keyboard, wl_seat::WlSeat},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_misc::{
    zwp_input_method_v2::client::{
        zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
        zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        zwp_input_method_v2::{self, ZwpInputMethodV2},
        zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
    },
    zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    },
};

const WIDTH: usize = 10;
const HEIGHT: usize = 300;

fn main() -> Result<(), Box<dyn Error>> {
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut event_queue) = registry_queue_init::<App>(&conn).unwrap();
    let qh = event_queue.handle();

    let seat: WlSeat = globals.bind(&qh, 1..=1, ())?;
    let vk_mgr: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ())?;
    let im_mgr: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ())?;
    let compositor: WlCompositor = globals.bind(&qh, 4..=4, ())?;
    let shm: WlShm = globals.bind(&qh, 1..=1, ())?;

    let input_method = im_mgr.get_input_method(&seat, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let mut shm_pool = SlotPool::new((WIDTH * HEIGHT * 4) as usize, &Provider(shm))?;
    let initial_buffer = create_buffer(&mut shm_pool).0;

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        compositor,
        shm_pool,
        input_method,
        pending_active: false,
        open_popup: None,
        grabbed_keyboard: None,
        virtual_keyboard: vk_mgr.create_virtual_keyboard(&seat, &qh, ()),
        surface,
        buffer: initial_buffer,
    };

    loop {
        event_queue.blocking_dispatch(&mut app).unwrap();
    }
}

struct App {
    registry_state: RegistryState,
    compositor: WlCompositor,
    shm_pool: SlotPool,
    pending_active: bool,
    input_method: ZwpInputMethodV2,
    virtual_keyboard: ZwpVirtualKeyboardV1,
    grabbed_keyboard: Option<GrabbedKeyboard>,
    open_popup: Option<OpenPopup>,
    surface: WlSurface,
    buffer: Buffer,
}

// Handle IME activation/deactivation by grabbing/releasing keyboard.
impl Dispatch<ZwpInputMethodV2, ()> for App {
    fn event(
        state: &mut Self,
        proxy: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        println!("{event:?}");
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = true,
            zwp_input_method_v2::Event::Deactivate => state.pending_active = false,
            zwp_input_method_v2::Event::Done => {
                if state.pending_active {
                    if state.grabbed_keyboard.is_none() {
                        state.grabbed_keyboard =
                            Some(GrabbedKeyboard(proxy.grab_keyboard(qhandle, ())));
                    }
                } else {
                    // Drop the grab if we have one.
                    state.grabbed_keyboard = None;
                }
            }
            _ => {}
        }
    }
}
struct GrabbedKeyboard(ZwpInputMethodKeyboardGrabV2);
impl Drop for GrabbedKeyboard {
    fn drop(&mut self) {
        self.0.release();
    }
}

// Handle key events with the grabbed keyboard.
impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        println!("{event:?}");
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Key {
                serial: _,
                time,
                key,
                state,
            } => {
                // On each keystroke, toggle the popup visibility.
                if state == WEnum::Value(wl_keyboard::KeyState::Pressed) {
                    if app.open_popup.is_some() {
                        app.open_popup = None
                    } else {
                        if false {
                            // ENABLE THIS AND EVERYTHING WORKS!
                            app.surface.destroy();
                            app.surface = app.compositor.create_surface(qhandle, ());
                        }
                        app.open_popup = Some(OpenPopup(app.input_method.get_input_popup_surface(
                            &app.surface,
                            qhandle,
                            (),
                        )));
                        draw(
                            &mut app.buffer,
                            &mut app.surface,
                            &mut app.shm_pool,
                            qhandle,
                        );
                    }
                }
                // Also pass the keystroke through to the app via VK.
                app.virtual_keyboard.key(time, key, state.into());
            }

            // Pass other events through to the app via VK.
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                app.virtual_keyboard.keymap(format.into(), fd.as_fd(), size);
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                app.virtual_keyboard
                    .modifiers(mods_depressed, mods_latched, mods_locked, group);
            }

            _ => {}
        }
    }
}
struct OpenPopup(ZwpInputPopupSurfaceV2);
impl Drop for OpenPopup {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

// Drawing and buffer management.
pub fn draw_into(data: &mut [u8]) {
    static DRAW_COUNT: AtomicUsize = AtomicUsize::new(0);
    let count = DRAW_COUNT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);

    const RED: [u8; 4] = [0u8, 0, 255, 255];
    const BLUE: [u8; 4] = [255u8, 0, 0, 255];
    for (index, pix) in data.chunks_exact_mut(4).enumerate() {
        pix.copy_from_slice(if index / WIDTH < count / 10 {
            &RED
        } else {
            &BLUE
        });
    }
}

fn draw(buffer: &mut Buffer, surface: &WlSurface, shm: &mut SlotPool, qh: &QueueHandle<App>) {
    if let Some(data) = buffer.canvas(shm) {
        draw_into(data);
    } else {
        let (newbuf, data) = create_buffer(shm);
        draw_into(data);
        *buffer = newbuf;
    };
    buffer.attach_to(surface).expect("attach");
    surface.damage_buffer(0, 0, WIDTH as i32, HEIGHT as i32);
    surface.frame(qh, ());
    surface.commit();
}

impl Dispatch<WlCallback, ()> for App {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        draw(
            &mut state.buffer,
            &mut state.surface,
            &mut state.shm_pool,
            qhandle,
        )
    }
}

fn create_buffer(shm: &mut SlotPool) -> (Buffer, &mut [u8]) {
    shm.create_buffer(
        WIDTH as i32,
        HEIGHT as i32,
        (WIDTH * 4) as i32,
        wl_shm::Format::Argb8888,
    )
    .expect("create buffer")
}

// Dumb framework boilerplate.
delegate_registry!(App);
delegate_noop!(App: ignore ZwpInputMethodManagerV2);
delegate_noop!(App: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(App: ignore ZwpVirtualKeyboardV1);
delegate_noop!(App: ignore WlSeat);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore ZwpInputPopupSurfaceV2);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlBuffer);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers!();
}

struct Provider<T>(T);
impl<T: Proxy, const N: u32> ProvidesBoundGlobal<T, N> for Provider<T> {
    fn bound_global(&self) -> Result<T, smithay_client_toolkit::error::GlobalError> {
        Ok(self.0.clone())
    }
}
