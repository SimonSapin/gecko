/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! A windowing implementation using glutin.

use NestedEventLoopListener;
use compositing::compositor_thread::{self, CompositorProxy, CompositorReceiver};
use compositing::windowing::{MouseWindowEvent, WindowNavigateMsg};
use compositing::windowing::{WindowEvent, WindowMethods};
use euclid::{Point2D, Size2D, TypedPoint2D};
use euclid::scale_factor::ScaleFactor;
use euclid::size::TypedSize2D;
#[cfg(target_os = "windows")]
use gdi32;
use gfx_traits::DevicePixel;
use gleam::gl;
use glutin;
use glutin::{Api, ElementState, Event, GlRequest, MouseButton, MouseScrollDelta, VirtualKeyCode};
use glutin::{ScanCode, TouchPhase};
#[cfg(target_os = "macos")]
use glutin::os::macos::{ActivationPolicy, WindowBuilderExt};
use msg::constellation_msg::{self, Key};
use msg::constellation_msg::{ALT, CONTROL, KeyState, NONE, SHIFT, SUPER};
use net_traits::net_error_list::NetError;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use osmesa_sys;
use script_traits::{TouchEventType, TouchpadPressurePhase};
use std::cell::{Cell, RefCell};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::mem;
use std::os::raw::c_void;
use std::ptr;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};
use style_traits::cursor::Cursor;
use url::Url;
#[cfg(target_os = "windows")]
use user32;
use util::geometry::ScreenPx;
use util::opts;
use util::prefs::PREFS;
use util::resource_files;
#[cfg(target_os = "windows")]
use winapi;

static mut G_NESTED_EVENT_LOOP_LISTENER: Option<*mut (NestedEventLoopListener + 'static)> = None;

bitflags! {
    flags KeyModifiers: u8 {
        const LEFT_CONTROL = 1,
        const RIGHT_CONTROL = 2,
        const LEFT_SHIFT = 4,
        const RIGHT_SHIFT = 8,
        const LEFT_ALT = 16,
        const RIGHT_ALT = 32,
        const LEFT_SUPER = 64,
        const RIGHT_SUPER = 128,
    }
}

// Some shortcuts use Cmd on Mac and Control on other systems.
#[cfg(target_os = "macos")]
const CMD_OR_CONTROL: constellation_msg::KeyModifiers = SUPER;
#[cfg(not(target_os = "macos"))]
const CMD_OR_CONTROL: constellation_msg::KeyModifiers = CONTROL;

// Some shortcuts use Cmd on Mac and Alt on other systems.
#[cfg(target_os = "macos")]
const CMD_OR_ALT: constellation_msg::KeyModifiers = SUPER;
#[cfg(not(target_os = "macos"))]
const CMD_OR_ALT: constellation_msg::KeyModifiers = ALT;

// This should vary by zoom level and maybe actual text size (focused or under cursor)
const LINE_HEIGHT: f32 = 38.0;

const MULTISAMPLES: u16 = 16;

#[cfg(target_os = "macos")]
fn builder_with_platform_options(mut builder: glutin::WindowBuilder) -> glutin::WindowBuilder {
    if opts::get().headless || opts::get().output_file.is_some() {
        // Prevent the window from showing in Dock.app, stealing focus,
        // or appearing at all when running in headless mode or generating an
        // output file.
        builder = builder.with_activation_policy(ActivationPolicy::Prohibited)
    }
    builder.with_app_name(String::from("Servo"))
}

#[cfg(not(target_os = "macos"))]
fn builder_with_platform_options(builder: glutin::WindowBuilder) -> glutin::WindowBuilder {
    builder
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct HeadlessContext {
    width: u32,
    height: u32,
    _context: osmesa_sys::OSMesaContext,
    _buffer: Vec<u32>,
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct HeadlessContext {
    width: u32,
    height: u32,
}

impl HeadlessContext {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn new(width: u32, height: u32) -> HeadlessContext {
        let mut attribs = Vec::new();

        attribs.push(osmesa_sys::OSMESA_PROFILE);
        attribs.push(osmesa_sys::OSMESA_CORE_PROFILE);
        attribs.push(osmesa_sys::OSMESA_CONTEXT_MAJOR_VERSION);
        attribs.push(3);
        attribs.push(osmesa_sys::OSMESA_CONTEXT_MINOR_VERSION);
        attribs.push(3);
        attribs.push(0);

        let context = unsafe {
            osmesa_sys::OSMesaCreateContextAttribs(attribs.as_ptr(), ptr::null_mut())
        };

        assert!(!context.is_null());

        let mut buffer = vec![0; (width * height) as usize];

        unsafe {
            let ret = osmesa_sys::OSMesaMakeCurrent(context,
                                                    buffer.as_mut_ptr() as *mut _,
                                                    gl::UNSIGNED_BYTE,
                                                    width as i32,
                                                    height as i32);
            assert!(ret != 0);
        };

        HeadlessContext {
            width: width,
            height: height,
            _context: context,
            _buffer: buffer,
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn new(width: u32, height: u32) -> HeadlessContext {
        HeadlessContext {
            width: width,
            height: height,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn get_proc_address(s: &str) -> *const c_void {
        let c_str = CString::new(s).expect("Unable to create CString");
        unsafe {
            mem::transmute(osmesa_sys::OSMesaGetProcAddress(c_str.as_ptr()))
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn get_proc_address(_: &str) -> *const c_void {
        ptr::null() as *const _
    }
}

enum WindowKind {
    Window(glutin::Window),
    Headless(HeadlessContext),
}

/// The type of a window.
pub struct Window {
    kind: WindowKind,

    mouse_down_button: Cell<Option<glutin::MouseButton>>,
    mouse_down_point: Cell<Point2D<i32>>,
    event_queue: RefCell<Vec<WindowEvent>>,

    mouse_pos: Cell<Point2D<i32>>,
    key_modifiers: Cell<KeyModifiers>,
    current_url: RefCell<Option<Url>>,

    /// The contents of the last ReceivedCharacter event for use in a subsequent KeyEvent.
    pending_key_event_char: Cell<Option<char>>,
    /// The list of keys that have been pressed but not yet released, to allow providing
    /// the equivalent ReceivedCharacter data as was received for the press event.
    pressed_key_map: RefCell<Vec<(ScanCode, char)>>,
}

#[cfg(not(target_os = "windows"))]
fn window_creation_scale_factor() -> ScaleFactor<f32, ScreenPx, DevicePixel> {
    ScaleFactor::new(1.0)
}

#[cfg(target_os = "windows")]
fn window_creation_scale_factor() -> ScaleFactor<f32, ScreenPx, DevicePixel> {
        let hdc = unsafe { user32::GetDC(::std::ptr::null_mut()) };
        let ppi = unsafe { gdi32::GetDeviceCaps(hdc, winapi::wingdi::LOGPIXELSY) };
        ScaleFactor::new(ppi as f32 / 96.0)
}


impl Window {
    pub fn new(is_foreground: bool,
               window_size: TypedSize2D<u32, ScreenPx>,
               parent: Option<glutin::WindowID>) -> Rc<Window> {
        let win_size: TypedSize2D<u32, DevicePixel> =
            (window_size.to_f32() * window_creation_scale_factor())
                .to_uint().cast().expect("Window size should fit in u32");
        let width = win_size.to_untyped().width;
        let height = win_size.to_untyped().height;

        // If there's no chrome, start off with the window invisible. It will be set to visible in
        // `load_end()`. This avoids an ugly flash of unstyled content (especially important since
        // unstyled content is white and chrome often has a transparent background). See issue
        // #9996.
        let visible = is_foreground && !opts::get().no_native_titlebar;

        let window_kind = if opts::get().headless {
            WindowKind::Headless(HeadlessContext::new(width, height))
        } else {
            let mut builder =
                glutin::WindowBuilder::new().with_title("Servo".to_string())
                                            .with_decorations(!opts::get().no_native_titlebar)
                                            .with_transparency(opts::get().no_native_titlebar)
                                            .with_dimensions(width, height)
                                            .with_gl(Window::gl_version())
                                            .with_visibility(visible)
                                            .with_parent(parent)
                                            .with_multitouch();

            if let Ok(mut icon_path) = resource_files::resources_dir_path() {
                icon_path.push("servo.png");
                builder = builder.with_icon(icon_path);
            }

            if opts::get().enable_vsync {
                builder = builder.with_vsync();
            }

            if opts::get().use_msaa {
                builder = builder.with_multisampling(MULTISAMPLES)
            }

            builder = builder_with_platform_options(builder);

            let mut glutin_window = builder.build().expect("Failed to create window.");

            unsafe { glutin_window.make_current().expect("Failed to make context current!") }

            glutin_window.set_window_resize_callback(Some(Window::nested_window_resize as fn(u32, u32)));

            WindowKind::Window(glutin_window)
        };

        Window::load_gl_functions(&window_kind);

        if opts::get().headless {
            // Print some information about the headless renderer that
            // can be useful in diagnosing CI failures on build machines.
            println!("{}", gl::get_string(gl::VENDOR));
            println!("{}", gl::get_string(gl::RENDERER));
            println!("{}", gl::get_string(gl::VERSION));
        }

        let window = Window {
            kind: window_kind,
            event_queue: RefCell::new(vec!()),
            mouse_down_button: Cell::new(None),
            mouse_down_point: Cell::new(Point2D::new(0, 0)),

            mouse_pos: Cell::new(Point2D::new(0, 0)),
            key_modifiers: Cell::new(KeyModifiers::empty()),
            current_url: RefCell::new(None),

            pending_key_event_char: Cell::new(None),
            pressed_key_map: RefCell::new(vec![]),
        };

        gl::clear_color(0.6, 0.6, 0.6, 1.0);
        gl::clear(gl::COLOR_BUFFER_BIT);
        gl::finish();
        window.present();

        Rc::new(window)
    }

    pub fn platform_window(&self) -> glutin::WindowID {
        match self.kind {
            WindowKind::Window(ref window) => {
                unsafe { glutin::WindowID::new(window.platform_window()) }
            }
            WindowKind::Headless(..) => {
                unreachable!();
            }
        }
    }

    fn nested_window_resize(width: u32, height: u32) {
        unsafe {
            match G_NESTED_EVENT_LOOP_LISTENER {
                None => {}
                Some(listener) => {
                    (*listener).handle_event_from_nested_event_loop(
                        WindowEvent::Resize(TypedSize2D::new(width, height)));
                }
            }
        }
    }

    #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
    fn gl_version() -> GlRequest {
        return GlRequest::Specific(Api::OpenGl, (3, 2));
    }

    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    fn gl_version() -> GlRequest {
        GlRequest::Specific(Api::OpenGlEs, (3, 0))
    }

    #[cfg(not(target_os = "android"))]
    fn load_gl_functions(window_kind: &WindowKind) {
        match window_kind {
            &WindowKind::Window(ref window) => {
                gl::load_with(|s| window.get_proc_address(s) as *const c_void);
            }
            &WindowKind::Headless(..) => {
                gl::load_with(|s| {
                    HeadlessContext::get_proc_address(s)
                });
            }
        }
    }

    #[cfg(target_os = "android")]
    fn load_gl_functions(_: &WindowKind) {
    }

    fn handle_window_event(&self, event: glutin::Event) -> bool {
        match event {
            Event::ReceivedCharacter(ch) => {
                if !ch.is_control() {
                    self.pending_key_event_char.set(Some(ch));
                }
            }
            Event::KeyboardInput(element_state, scan_code, Some(virtual_key_code)) => {
                match virtual_key_code {
                    VirtualKeyCode::LControl => self.toggle_modifier(LEFT_CONTROL),
                    VirtualKeyCode::RControl => self.toggle_modifier(RIGHT_CONTROL),
                    VirtualKeyCode::LShift => self.toggle_modifier(LEFT_SHIFT),
                    VirtualKeyCode::RShift => self.toggle_modifier(RIGHT_SHIFT),
                    VirtualKeyCode::LAlt => self.toggle_modifier(LEFT_ALT),
                    VirtualKeyCode::RAlt => self.toggle_modifier(RIGHT_ALT),
                    VirtualKeyCode::LWin => self.toggle_modifier(LEFT_SUPER),
                    VirtualKeyCode::RWin => self.toggle_modifier(RIGHT_SUPER),
                    _ => {}
                }

                let ch = match element_state {
                    ElementState::Pressed => {
                        // Retrieve any previosly stored ReceivedCharacter value.
                        // Store the association between the scan code and the actual
                        // character value, if there is one.
                        let ch = self.pending_key_event_char
                                     .get()
                                     .and_then(|ch| filter_nonprintable(ch, virtual_key_code));
                        self.pending_key_event_char.set(None);
                        if let Some(ch) = ch {
                            self.pressed_key_map.borrow_mut().push((scan_code, ch));
                        }
                        ch
                    }

                    ElementState::Released => {
                        // Retrieve the associated character value for this release key,
                        // if one was previously stored.
                        let idx = self.pressed_key_map
                                      .borrow()
                                      .iter()
                                      .position(|&(code, _)| code == scan_code);
                        idx.map(|idx| self.pressed_key_map.borrow_mut().swap_remove(idx).1)
                    }
                };

                if let Ok(key) = Window::glutin_key_to_script_key(virtual_key_code) {
                    let state = match element_state {
                        ElementState::Pressed => KeyState::Pressed,
                        ElementState::Released => KeyState::Released,
                    };
                    let modifiers = Window::glutin_mods_to_script_mods(self.key_modifiers.get());
                    self.event_queue.borrow_mut().push(WindowEvent::KeyEvent(ch, key, state, modifiers));
                }
            }
            Event::KeyboardInput(_, _, None) => {
                debug!("Keyboard input without virtual key.");
            }
            Event::Resized(width, height) => {
                self.event_queue.borrow_mut().push(WindowEvent::Resize(TypedSize2D::new(width, height)));
            }
            Event::MouseInput(element_state, mouse_button, pos) => {
                if mouse_button == MouseButton::Left ||
                   mouse_button == MouseButton::Right {
                    match pos {
                        Some((x, y)) => {
                            self.mouse_pos.set(Point2D::new(x, y));
                            self.event_queue.borrow_mut().push(
                                WindowEvent::MouseWindowMoveEventClass(TypedPoint2D::new(x as f32, y as f32)));
                            self.handle_mouse(mouse_button, element_state, x, y);
                        }
                        None => {
                            let mouse_pos = self.mouse_pos.get();
                            self.handle_mouse(mouse_button, element_state, mouse_pos.x, mouse_pos.y);
                        }
                    }
                }
            }
            Event::MouseMoved(x, y) => {
                self.mouse_pos.set(Point2D::new(x, y));
                self.event_queue.borrow_mut().push(
                    WindowEvent::MouseWindowMoveEventClass(TypedPoint2D::new(x as f32, y as f32)));
            }
            Event::MouseWheel(delta, phase) => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => (dx, dy * LINE_HEIGHT),
                    MouseScrollDelta::PixelDelta(dx, dy) => (dx, dy),
                };
                let phase = glutin_phase_to_touch_event_type(phase);
                self.scroll_window(dx, dy, phase);
            },
            Event::Touch(touch) => {
                use script_traits::TouchId;

                let phase = glutin_phase_to_touch_event_type(touch.phase);
                let id = TouchId(touch.id as i32);
                let point = TypedPoint2D::new(touch.location.0 as f32, touch.location.1 as f32);
                self.event_queue.borrow_mut().push(WindowEvent::Touch(phase, id, point));
            }
            Event::TouchpadPressure(pressure, stage) => {
                let m = self.mouse_pos.get();
                let point = TypedPoint2D::new(m.x as f32, m.y as f32);
                let phase = glutin_pressure_stage_to_touchpad_pressure_phase(stage);
                self.event_queue.borrow_mut().push(WindowEvent::TouchpadPressure(point, pressure, phase));
            }
            Event::Refresh => {
                self.event_queue.borrow_mut().push(WindowEvent::Refresh);
            }
            Event::Closed => {
                return true
            }
            _ => {}
        }

        false
    }

    fn toggle_modifier(&self, modifier: KeyModifiers) {
        let mut modifiers = self.key_modifiers.get();
        modifiers.toggle(modifier);
        self.key_modifiers.set(modifiers);
    }

    /// Helper function to send a scroll event.
    fn scroll_window(&self, mut dx: f32, mut dy: f32, phase: TouchEventType) {
        // Scroll events snap to the major axis of movement, with vertical
        // preferred over horizontal.
        if dy.abs() >= dx.abs() {
            dx = 0.0;
        } else {
            dy = 0.0;
        }
        let mouse_pos = self.mouse_pos.get();
        let event = WindowEvent::Scroll(TypedPoint2D::new(dx as f32, dy as f32),
                                        TypedPoint2D::new(mouse_pos.x as i32, mouse_pos.y as i32),
                                        phase);
        self.event_queue.borrow_mut().push(event);
    }

    /// Helper function to handle a click
    fn handle_mouse(&self, button: glutin::MouseButton, action: glutin::ElementState, x: i32, y: i32) {
        use script_traits::MouseButton;

        // FIXME(tkuehn): max pixel dist should be based on pixel density
        let max_pixel_dist = 10f64;
        let event = match action {
            ElementState::Pressed => {
                self.mouse_down_point.set(Point2D::new(x, y));
                self.mouse_down_button.set(Some(button));
                MouseWindowEvent::MouseDown(MouseButton::Left, TypedPoint2D::new(x as f32, y as f32))
            }
            ElementState::Released => {
                let mouse_up_event = MouseWindowEvent::MouseUp(MouseButton::Left,
                                                               TypedPoint2D::new(x as f32, y as f32));
                match self.mouse_down_button.get() {
                    None => mouse_up_event,
                    Some(but) if button == but => {
                        let pixel_dist = self.mouse_down_point.get() - Point2D::new(x, y);
                        let pixel_dist = ((pixel_dist.x * pixel_dist.x +
                                           pixel_dist.y * pixel_dist.y) as f64).sqrt();
                        if pixel_dist < max_pixel_dist {
                            self.event_queue.borrow_mut().push(WindowEvent::MouseWindowEventClass(mouse_up_event));
                            MouseWindowEvent::Click(MouseButton::Left, TypedPoint2D::new(x as f32, y as f32))
                        } else {
                            mouse_up_event
                        }
                    },
                    Some(_) => mouse_up_event,
                }
            }
        };
        self.event_queue.borrow_mut().push(WindowEvent::MouseWindowEventClass(event));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn handle_next_event(&self) -> bool {
        match self.kind {
            WindowKind::Window(ref window) => {
                let event = match window.wait_events().next() {
                    None => {
                        warn!("Window event stream closed.");
                        return true;
                    },
                    Some(event) => event,
                };
                let mut close = self.handle_window_event(event);
                if !close {
                    while let Some(event) = window.poll_events().next() {
                        if self.handle_window_event(event) {
                            close = true;
                            break
                        }
                    }
                }
                close
            }
            WindowKind::Headless(..) => {
                false
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn handle_next_event(&self) -> bool {
        match self.kind {
            WindowKind::Window(ref window) => {
                let event = match window.wait_events().next() {
                    None => {
                        warn!("Window event stream closed.");
                        return true;
                    },
                    Some(event) => event,
                };
                let mut close = self.handle_window_event(event);
                if !close {
                    while let Some(event) = window.poll_events().next() {
                        if self.handle_window_event(event) {
                            close = true;
                            break
                        }
                    }
                }
                close
            }
            WindowKind::Headless(..) => {
                false
            }
        }
    }

    pub fn wait_events(&self) -> Vec<WindowEvent> {
        use std::mem;

        let mut events = mem::replace(&mut *self.event_queue.borrow_mut(), Vec::new());
        let mut close_event = false;

        // When writing to a file then exiting, use event
        // polling so that we don't block on a GUI event
        // such as mouse click.
        if opts::get().output_file.is_some() || opts::get().exit_after_load || opts::get().headless {
            match self.kind {
                WindowKind::Window(ref window) => {
                    while let Some(event) = window.poll_events().next() {
                        close_event = self.handle_window_event(event) || close_event;
                    }
                }
                WindowKind::Headless(..) => {}
            }
        } else {
            close_event = self.handle_next_event();
        }

        if close_event {
            events.push(WindowEvent::Quit)
        }

        events.extend(mem::replace(&mut *self.event_queue.borrow_mut(), Vec::new()).into_iter());
        events
    }

    pub unsafe fn set_nested_event_loop_listener(
            &self,
            listener: *mut (NestedEventLoopListener + 'static)) {
        G_NESTED_EVENT_LOOP_LISTENER = Some(listener)
    }

    pub unsafe fn remove_nested_event_loop_listener(&self) {
        G_NESTED_EVENT_LOOP_LISTENER = None
    }

    fn glutin_key_to_script_key(key: glutin::VirtualKeyCode) -> Result<constellation_msg::Key, ()> {
        // TODO(negge): add more key mappings
        match key {
            VirtualKeyCode::A => Ok(Key::A),
            VirtualKeyCode::B => Ok(Key::B),
            VirtualKeyCode::C => Ok(Key::C),
            VirtualKeyCode::D => Ok(Key::D),
            VirtualKeyCode::E => Ok(Key::E),
            VirtualKeyCode::F => Ok(Key::F),
            VirtualKeyCode::G => Ok(Key::G),
            VirtualKeyCode::H => Ok(Key::H),
            VirtualKeyCode::I => Ok(Key::I),
            VirtualKeyCode::J => Ok(Key::J),
            VirtualKeyCode::K => Ok(Key::K),
            VirtualKeyCode::L => Ok(Key::L),
            VirtualKeyCode::M => Ok(Key::M),
            VirtualKeyCode::N => Ok(Key::N),
            VirtualKeyCode::O => Ok(Key::O),
            VirtualKeyCode::P => Ok(Key::P),
            VirtualKeyCode::Q => Ok(Key::Q),
            VirtualKeyCode::R => Ok(Key::R),
            VirtualKeyCode::S => Ok(Key::S),
            VirtualKeyCode::T => Ok(Key::T),
            VirtualKeyCode::U => Ok(Key::U),
            VirtualKeyCode::V => Ok(Key::V),
            VirtualKeyCode::W => Ok(Key::W),
            VirtualKeyCode::X => Ok(Key::X),
            VirtualKeyCode::Y => Ok(Key::Y),
            VirtualKeyCode::Z => Ok(Key::Z),

            VirtualKeyCode::Numpad0 => Ok(Key::Kp0),
            VirtualKeyCode::Numpad1 => Ok(Key::Kp1),
            VirtualKeyCode::Numpad2 => Ok(Key::Kp2),
            VirtualKeyCode::Numpad3 => Ok(Key::Kp3),
            VirtualKeyCode::Numpad4 => Ok(Key::Kp4),
            VirtualKeyCode::Numpad5 => Ok(Key::Kp5),
            VirtualKeyCode::Numpad6 => Ok(Key::Kp6),
            VirtualKeyCode::Numpad7 => Ok(Key::Kp7),
            VirtualKeyCode::Numpad8 => Ok(Key::Kp8),
            VirtualKeyCode::Numpad9 => Ok(Key::Kp9),

            VirtualKeyCode::Key0 => Ok(Key::Num0),
            VirtualKeyCode::Key1 => Ok(Key::Num1),
            VirtualKeyCode::Key2 => Ok(Key::Num2),
            VirtualKeyCode::Key3 => Ok(Key::Num3),
            VirtualKeyCode::Key4 => Ok(Key::Num4),
            VirtualKeyCode::Key5 => Ok(Key::Num5),
            VirtualKeyCode::Key6 => Ok(Key::Num6),
            VirtualKeyCode::Key7 => Ok(Key::Num7),
            VirtualKeyCode::Key8 => Ok(Key::Num8),
            VirtualKeyCode::Key9 => Ok(Key::Num9),

            VirtualKeyCode::Return => Ok(Key::Enter),
            VirtualKeyCode::Space => Ok(Key::Space),
            VirtualKeyCode::Escape => Ok(Key::Escape),
            VirtualKeyCode::Equals => Ok(Key::Equal),
            VirtualKeyCode::Minus => Ok(Key::Minus),
            VirtualKeyCode::Back => Ok(Key::Backspace),
            VirtualKeyCode::PageDown => Ok(Key::PageDown),
            VirtualKeyCode::PageUp => Ok(Key::PageUp),

            VirtualKeyCode::Insert => Ok(Key::Insert),
            VirtualKeyCode::Home => Ok(Key::Home),
            VirtualKeyCode::Delete => Ok(Key::Delete),
            VirtualKeyCode::End => Ok(Key::End),

            VirtualKeyCode::Left => Ok(Key::Left),
            VirtualKeyCode::Up => Ok(Key::Up),
            VirtualKeyCode::Right => Ok(Key::Right),
            VirtualKeyCode::Down => Ok(Key::Down),

            VirtualKeyCode::LShift => Ok(Key::LeftShift),
            VirtualKeyCode::LControl => Ok(Key::LeftControl),
            VirtualKeyCode::LAlt => Ok(Key::LeftAlt),
            VirtualKeyCode::LWin => Ok(Key::LeftSuper),
            VirtualKeyCode::RShift => Ok(Key::RightShift),
            VirtualKeyCode::RControl => Ok(Key::RightControl),
            VirtualKeyCode::RAlt => Ok(Key::RightAlt),
            VirtualKeyCode::RWin => Ok(Key::RightSuper),

            VirtualKeyCode::Apostrophe => Ok(Key::Apostrophe),
            VirtualKeyCode::Backslash => Ok(Key::Backslash),
            VirtualKeyCode::Comma => Ok(Key::Comma),
            VirtualKeyCode::Grave => Ok(Key::GraveAccent),
            VirtualKeyCode::LBracket => Ok(Key::LeftBracket),
            VirtualKeyCode::Period => Ok(Key::Period),
            VirtualKeyCode::RBracket => Ok(Key::RightBracket),
            VirtualKeyCode::Semicolon => Ok(Key::Semicolon),
            VirtualKeyCode::Slash => Ok(Key::Slash),
            VirtualKeyCode::Tab => Ok(Key::Tab),
            VirtualKeyCode::Subtract => Ok(Key::Minus),

            VirtualKeyCode::NavigateBackward => Ok(Key::NavigateBackward),
            VirtualKeyCode::NavigateForward => Ok(Key::NavigateForward),
            _ => Err(()),
        }
    }

    fn glutin_mods_to_script_mods(modifiers: KeyModifiers) -> constellation_msg::KeyModifiers {
        let mut result = constellation_msg::KeyModifiers::empty();
        if modifiers.intersects(LEFT_SHIFT | RIGHT_SHIFT) {
            result.insert(SHIFT);
        }
        if modifiers.intersects(LEFT_CONTROL | RIGHT_CONTROL) {
            result.insert(CONTROL);
        }
        if modifiers.intersects(LEFT_ALT | RIGHT_ALT) {
            result.insert(ALT);
        }
        if modifiers.intersects(LEFT_SUPER | RIGHT_SUPER) {
            result.insert(SUPER);
        }
        result
    }

    #[cfg(not(target_os = "win"))]
    fn platform_handle_key(&self, key: Key, mods: constellation_msg::KeyModifiers) {
        match (mods, key) {
            (CMD_OR_CONTROL, Key::LeftBracket) => {
                self.event_queue.borrow_mut().push(WindowEvent::Navigation(WindowNavigateMsg::Back));
            }
            (CMD_OR_CONTROL, Key::RightBracket) => {
                self.event_queue.borrow_mut().push(WindowEvent::Navigation(WindowNavigateMsg::Forward));
            }
            _ => {}
        }
    }

    #[cfg(target_os = "win")]
    fn platform_handle_key(&self, key: Key, mods: constellation_msg::KeyModifiers) {
    }
}

// WindowProxy is not implemented for android yet

#[cfg(target_os = "android")]
fn create_window_proxy(_: &Window) -> Option<glutin::WindowProxy> {
    None
}

#[cfg(not(target_os = "android"))]
fn create_window_proxy(window: &Window) -> Option<glutin::WindowProxy> {
    match window.kind {
        WindowKind::Window(ref window) => {
            Some(window.create_window_proxy())
        }
        WindowKind::Headless(..) => {
            None
        }
    }
}

impl WindowMethods for Window {
    fn framebuffer_size(&self) -> TypedSize2D<u32, DevicePixel> {
        match self.kind {
            WindowKind::Window(ref window) => {
                let scale_factor = window.hidpi_factor() as u32;
                // TODO(ajeffrey): can this fail?
                let (width, height) = window.get_inner_size().expect("Failed to get window inner size.");
                TypedSize2D::new(width * scale_factor, height * scale_factor)
            }
            WindowKind::Headless(ref context) => {
                TypedSize2D::new(context.width, context.height)
            }
        }
    }

    fn size(&self) -> TypedSize2D<f32, ScreenPx> {
        match self.kind {
            WindowKind::Window(ref window) => {
                // TODO(ajeffrey): can this fail?
                let (width, height) = window.get_inner_size().expect("Failed to get window inner size.");
                TypedSize2D::new(width as f32, height as f32)
            }
            WindowKind::Headless(ref context) => {
                TypedSize2D::new(context.width as f32, context.height as f32)
            }
        }
    }

    fn client_window(&self) -> (Size2D<u32>, Point2D<i32>) {
        match self.kind {
            WindowKind::Window(ref window) => {
                // TODO(ajeffrey): can this fail?
                let (width, height) = window.get_outer_size().expect("Failed to get window outer size.");
                let size = Size2D::new(width, height);
                // TODO(ajeffrey): can this fail?
                let (x, y) = window.get_position().expect("Failed to get window position.");
                let origin = Point2D::new(x as i32, y as i32);
                (size, origin)
            }
            WindowKind::Headless(ref context) => {
                let size = TypedSize2D::new(context.width, context.height);
                (size, Point2D::zero())
            }
        }

    }

    fn set_inner_size(&self, size: Size2D<u32>) {
        match self.kind {
            WindowKind::Window(ref window) => {
                window.set_inner_size(size.width as u32, size.height as u32)
            }
            WindowKind::Headless(..) => {}
        }
    }

    fn set_position(&self, point: Point2D<i32>) {
        match self.kind {
            WindowKind::Window(ref window) => {
                window.set_position(point.x, point.y)
            }
            WindowKind::Headless(..) => {}
        }
    }

    fn present(&self) {
        match self.kind {
            WindowKind::Window(ref window) => {
                if let Err(err) = window.swap_buffers() {
                    warn!("Failed to swap window buffers ({}).", err);
                }
            }
            WindowKind::Headless(..) => {}
        }
    }

    fn create_compositor_channel(&self)
                                 -> (Box<CompositorProxy + Send>, Box<CompositorReceiver>) {
        let (sender, receiver) = channel();

        let window_proxy = create_window_proxy(self);

        (box GlutinCompositorProxy {
             sender: sender,
             window_proxy: window_proxy,
         } as Box<CompositorProxy + Send>,
         box receiver as Box<CompositorReceiver>)
    }

    #[cfg(not(target_os = "windows"))]
    fn scale_factor(&self) -> ScaleFactor<f32, ScreenPx, DevicePixel> {
        match self.kind {
            WindowKind::Window(ref window) => {
                ScaleFactor::new(window.hidpi_factor())
            }
            WindowKind::Headless(..) => {
                ScaleFactor::new(1.0)
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn scale_factor(&self) -> ScaleFactor<f32, ScreenPx, DevicePixel> {
        let hdc = unsafe { user32::GetDC(::std::ptr::null_mut()) };
        let ppi = unsafe { gdi32::GetDeviceCaps(hdc, winapi::wingdi::LOGPIXELSY) };
        ScaleFactor::new(ppi as f32 / 96.0)
    }

    fn set_page_title(&self, title: Option<String>) {
        match self.kind {
            WindowKind::Window(ref window) => {
                let fallback_title: String = if let Some(ref current_url) = *self.current_url.borrow() {
                    current_url.to_string()
                } else {
                    String::from("Untitled")
                };

                let title = match title {
                    Some(ref title) if title.len() > 0 => &**title,
                    _ => &fallback_title,
                };
                let title = format!("{} - Servo", title);
                window.set_title(&title);
            }
            WindowKind::Headless(..) => {}
        }
    }

    fn set_page_url(&self, url: Url) {
        *self.current_url.borrow_mut() = Some(url);
    }

    fn status(&self, _: Option<String>) {
    }

    fn load_start(&self, _: bool, _: bool) {
    }

    fn load_end(&self, _: bool, _: bool, root: bool) {
        if root && opts::get().no_native_titlebar {
            match self.kind {
                WindowKind::Window(ref window) => {
                    window.show();
                }
                WindowKind::Headless(..) => {}
            }
        }
    }

    fn load_error(&self, _: NetError, _: String) {
    }

    fn head_parsed(&self) {
    }

    /// Has no effect on Android.
    fn set_cursor(&self, c: Cursor) {
        match self.kind {
            WindowKind::Window(ref window) => {
                use glutin::MouseCursor;

                let glutin_cursor = match c {
                    Cursor::None => MouseCursor::NoneCursor,
                    Cursor::Default => MouseCursor::Default,
                    Cursor::Pointer => MouseCursor::Hand,
                    Cursor::ContextMenu => MouseCursor::ContextMenu,
                    Cursor::Help => MouseCursor::Help,
                    Cursor::Progress => MouseCursor::Progress,
                    Cursor::Wait => MouseCursor::Wait,
                    Cursor::Cell => MouseCursor::Cell,
                    Cursor::Crosshair => MouseCursor::Crosshair,
                    Cursor::Text => MouseCursor::Text,
                    Cursor::VerticalText => MouseCursor::VerticalText,
                    Cursor::Alias => MouseCursor::Alias,
                    Cursor::Copy => MouseCursor::Copy,
                    Cursor::Move => MouseCursor::Move,
                    Cursor::NoDrop => MouseCursor::NoDrop,
                    Cursor::NotAllowed => MouseCursor::NotAllowed,
                    Cursor::Grab => MouseCursor::Grab,
                    Cursor::Grabbing => MouseCursor::Grabbing,
                    Cursor::EResize => MouseCursor::EResize,
                    Cursor::NResize => MouseCursor::NResize,
                    Cursor::NeResize => MouseCursor::NeResize,
                    Cursor::NwResize => MouseCursor::NwResize,
                    Cursor::SResize => MouseCursor::SResize,
                    Cursor::SeResize => MouseCursor::SeResize,
                    Cursor::SwResize => MouseCursor::SwResize,
                    Cursor::WResize => MouseCursor::WResize,
                    Cursor::EwResize => MouseCursor::EwResize,
                    Cursor::NsResize => MouseCursor::NsResize,
                    Cursor::NeswResize => MouseCursor::NeswResize,
                    Cursor::NwseResize => MouseCursor::NwseResize,
                    Cursor::ColResize => MouseCursor::ColResize,
                    Cursor::RowResize => MouseCursor::RowResize,
                    Cursor::AllScroll => MouseCursor::AllScroll,
                    Cursor::ZoomIn => MouseCursor::ZoomIn,
                    Cursor::ZoomOut => MouseCursor::ZoomOut,
                };
                window.set_cursor(glutin_cursor);
            }
            WindowKind::Headless(..) => {}
        }
    }

    fn set_favicon(&self, _: Url) {
    }

    fn prepare_for_composite(&self, _width: usize, _height: usize) -> bool {
        true
    }

    /// Helper function to handle keyboard events.
    fn handle_key(&self, ch: Option<char>, key: Key, mods: constellation_msg::KeyModifiers) {
        match (mods, ch, key) {
            (_, Some('+'), _) => {
                if mods & !SHIFT == CMD_OR_CONTROL {
                    self.event_queue.borrow_mut().push(WindowEvent::Zoom(1.1));
                } else if mods & !SHIFT == CMD_OR_CONTROL | ALT {
                    self.event_queue.borrow_mut().push(WindowEvent::PinchZoom(1.1));
                }
            }
            (CMD_OR_CONTROL, Some('-'), _) => {
                self.event_queue.borrow_mut().push(WindowEvent::Zoom(1.0 / 1.1));
            }
            (_, Some('-'), _) if mods == CMD_OR_CONTROL | ALT => {
                self.event_queue.borrow_mut().push(WindowEvent::PinchZoom(1.0 / 1.1));
            }
            (CMD_OR_CONTROL, Some('0'), _) => {
                self.event_queue.borrow_mut().push(WindowEvent::ResetZoom);
            }

            (NONE, None, Key::NavigateForward) => {
                self.event_queue.borrow_mut().push(WindowEvent::Navigation(WindowNavigateMsg::Forward));
            }
            (NONE, None, Key::NavigateBackward) => {
                self.event_queue.borrow_mut().push(WindowEvent::Navigation(WindowNavigateMsg::Back));
            }

            (NONE, None, Key::Escape) => {
                if let Some(true) = PREFS.get("shell.builtin-key-shortcuts.enabled").as_boolean() {
                    self.event_queue.borrow_mut().push(WindowEvent::Quit);
                }
            }

            (CMD_OR_ALT, None, Key::Right) => {
                self.event_queue.borrow_mut().push(WindowEvent::Navigation(WindowNavigateMsg::Forward));
            }
            (CMD_OR_ALT, None, Key::Left) => {
                self.event_queue.borrow_mut().push(WindowEvent::Navigation(WindowNavigateMsg::Back));
            }

            (NONE, None, Key::PageDown) |
            (NONE, Some(' '), _) => {
                self.scroll_window(0.0,
                                   -self.framebuffer_size()
                                        .to_f32()
                                        .to_untyped()
                                        .height + 2.0 * LINE_HEIGHT,
                                   TouchEventType::Move);
            }
            (NONE, None, Key::PageUp) |
            (SHIFT, Some(' '), _) => {
                self.scroll_window(0.0,
                                   self.framebuffer_size()
                                       .to_f32()
                                       .to_untyped()
                                       .height - 2.0 * LINE_HEIGHT,
                                   TouchEventType::Move);
            }
            (NONE, None, Key::Up) => {
                self.scroll_window(0.0, 3.0 * LINE_HEIGHT, TouchEventType::Move);
            }
            (NONE, None, Key::Down) => {
                self.scroll_window(0.0, -3.0 * LINE_HEIGHT, TouchEventType::Move);
            }
            (NONE, None, Key::Left) => {
                self.scroll_window(LINE_HEIGHT, 0.0, TouchEventType::Move);
            }
            (NONE, None, Key::Right) => {
                self.scroll_window(-LINE_HEIGHT, 0.0, TouchEventType::Move);
            }
            (CMD_OR_CONTROL, Some('r'), _) => {
                if let Some(true) = PREFS.get("shell.builtin-key-shortcuts.enabled").as_boolean() {
                    self.event_queue.borrow_mut().push(WindowEvent::Reload);
                }
            }
            (CMD_OR_CONTROL, Some('q'), _) => {
                if let Some(true) = PREFS.get("shell.builtin-key-shortcuts.enabled").as_boolean() {
                    self.event_queue.borrow_mut().push(WindowEvent::Quit);
                }
            }

            _ => {
                self.platform_handle_key(key, mods);
            }
        }
    }

    fn supports_clipboard(&self) -> bool {
        false
    }
}

struct GlutinCompositorProxy {
    sender: Sender<compositor_thread::Msg>,
    window_proxy: Option<glutin::WindowProxy>,
}

impl CompositorProxy for GlutinCompositorProxy {
    fn send(&self, msg: compositor_thread::Msg) {
        // Send a message and kick the OS event loop awake.
        if let Err(err) = self.sender.send(msg) {
            warn!("Failed to send response ({}).", err);
        }
        if let Some(ref window_proxy) = self.window_proxy {
            window_proxy.wakeup_event_loop()
        }
    }
    fn clone_compositor_proxy(&self) -> Box<CompositorProxy + Send> {
        box GlutinCompositorProxy {
            sender: self.sender.clone(),
            window_proxy: self.window_proxy.clone(),
        } as Box<CompositorProxy + Send>
    }
}

fn glutin_phase_to_touch_event_type(phase: TouchPhase) -> TouchEventType {
    match phase {
        TouchPhase::Started => TouchEventType::Down,
        TouchPhase::Moved => TouchEventType::Move,
        TouchPhase::Ended => TouchEventType::Up,
        TouchPhase::Cancelled => TouchEventType::Cancel,
    }
}

fn glutin_pressure_stage_to_touchpad_pressure_phase(stage: i64) -> TouchpadPressurePhase {
    if stage < 1 {
        TouchpadPressurePhase::BeforeClick
    } else if stage < 2 {
        TouchpadPressurePhase::AfterFirstClick
    } else {
        TouchpadPressurePhase::AfterSecondClick
    }
}

fn filter_nonprintable(ch: char, key_code: VirtualKeyCode) -> Option<char> {
    use glutin::VirtualKeyCode::*;
    match key_code {
        Escape |
        F1 |
        F2 |
        F3 |
        F4 |
        F5 |
        F6 |
        F7 |
        F8 |
        F9 |
        F10 |
        F11 |
        F12 |
        F13 |
        F14 |
        F15 |
        Snapshot |
        Scroll |
        Pause |
        Insert |
        Home |
        Delete |
        End |
        PageDown |
        PageUp |
        Left |
        Up |
        Right |
        Down |
        Back |
        LAlt |
        LControl |
        LMenu |
        LShift |
        LWin |
        Mail |
        MediaSelect |
        MediaStop |
        Mute |
        MyComputer |
        NavigateForward |
        NavigateBackward |
        NextTrack |
        NoConvert |
        PlayPause |
        Power |
        PrevTrack |
        RAlt |
        RControl |
        RMenu |
        RShift |
        RWin |
        Sleep |
        Stop |
        VolumeDown |
        VolumeUp |
        Wake |
        WebBack |
        WebFavorites |
        WebForward |
        WebHome |
        WebRefresh |
        WebSearch |
        WebStop => None,
        _ => Some(ch),
    }
}

// These functions aren't actually called. They are here as a link
// hack because Skia references them.

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn glBindVertexArrayOES(_array: usize)
{
    unimplemented!()
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn glDeleteVertexArraysOES(_n: isize, _arrays: *const ())
{
    unimplemented!()
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn glGenVertexArraysOES(_n: isize, _arrays: *const ())
{
    unimplemented!()
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn glRenderbufferStorageMultisampleIMG(_: isize, _: isize, _: isize, _: isize, _: isize)
{
    unimplemented!()
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn glFramebufferTexture2DMultisampleIMG(_: isize, _: isize, _: isize, _: isize, _: isize, _: isize)
{
    unimplemented!()
}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn glDiscardFramebufferEXT(_: isize, _: isize, _: *const ())
{
    unimplemented!()
}
