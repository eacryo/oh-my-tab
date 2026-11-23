//! The Dock hover observer.
//!
//! One dedicated thread owns everything Accessibility-related for the Dock: the `AXObserver`
//! subscription, the Dock process's window list element and every attribute read. The AppKit main
//! thread never calls AX here, so an unresponsive Dock cannot stall the UI; the thread only ever
//! sends plain data (`DockEvent`) to the main thread.
//!
//! The hover signal is the Dock's own Accessibility change notification on its list element:
//! `AXSelectedChildrenChanged` fires when the pointer moves between tiles, and `AXSelectedChildren`
//! names the hovered tile. The subscription is rebuilt when the Dock restarts or its list element is
//! invalidated (a Dock rebuild drops the notification silently), and a low-frequency poll re-reads
//! the selection so a missed notification still surfaces.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use crate::dock_preview::layout::{dock_edge_from_frames, dock_revealed, DockEdge, Rect};
use crate::event_tap::{kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent};
use crate::ffi::{
    make_nsstring, nsstring_to_rust, release_obj, AXObserverAddNotification, AXObserverCreate,
    AXObserverGetRunLoopSource, AXObserverRemoveNotification, AXUIElementCopyAttributeValue,
    AXUIElementCreateApplication, AXUIElementRef, AXValueGetValue, AxObserverRef, CFArrayGetCount,
    CFArrayGetValueAtIndex, CFGetTypeID, CFRelease, CFRetain, CFRunLoopRemoveSource,
    CFRunLoopRunInMode, CFRunLoopWakeUp, RunLoopHandle,
};
use crate::log_debug;
use crate::window_collector::cf_string_new;

const DOCK_BUNDLE_ID: &str = "com.apple.dock";
const SELECTION_CHANGED: &str = "AXSelectedChildrenChanged";
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const HEALTH_INTERVAL: Duration = Duration::from_secs(2);
const K_AX_VALUE_CG_POINT: i32 = 1;
const K_AX_VALUE_CG_SIZE: i32 = 2;
/// kCFStringEncodingUTF8, the encoding CFStringGetCString expects here.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[repr(C)]
struct CgPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
struct CgSize {
    w: f64,
    h: f64,
}

/// What the Dock says is under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Selection {
    /// An application tile that is running (so it has windows to preview).
    App {
        pid: i32,
        bundle: String,
        title: String,
        icon: Rect,
        edge: DockEdge,
    },
    /// A Dock tile that is not an application (separator, Trash, folder, recent stack).
    NonApp,
    /// Nothing selected: the pointer left the Dock, or the Dock hid.
    None,
}

/// One observer turn: the selection plus the Dock state at that moment.
#[derive(Clone, Debug)]
pub(crate) struct DockEvent {
    pub(crate) session: u64,
    pub(crate) selection: Selection,
    pub(crate) list_frame: Option<Rect>,
    pub(crate) screen_frame: Option<Rect>,
    pub(crate) revealed: bool,
}

pub(crate) type EventSink = Arc<dyn Fn(DockEvent) + Send + Sync>;

/// The e2e view of the Dock: which tiles exist and where, so a scenario can move the real pointer
/// onto a known tile without deriving coordinates itself.
#[derive(Clone, Debug, Default)]
pub(crate) struct DockSnapshot {
    pub(crate) list: Option<Rect>,
    pub(crate) screen: Option<Rect>,
    pub(crate) revealed: bool,
    pub(crate) edge: Option<DockEdge>,
    pub(crate) items: Vec<(String, Rect)>,
}

static SNAPSHOT: LazyLock<Mutex<DockSnapshot>> =
    LazyLock::new(|| Mutex::new(DockSnapshot::default()));
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The latest Dock geometry, for `--e2e-state`.
pub(crate) fn current_snapshot() -> DockSnapshot {
    SNAPSHOT.lock().unwrap().clone()
}

enum Command {
    Rebuild(u64),
    /// Tear the subscription down but keep the thread for the next start. Reusing one thread is
    /// what makes a fast off/on toggle serial: no second observer can register before the first
    /// one has torn down.
    Pause,
}

/// Owner handle for the observer thread.
pub(crate) struct ObserverHandle {
    commands: Sender<Command>,
    wake: RunLoopHandle,
}

impl ObserverHandle {
    pub(crate) fn rebuild(&self, session: u64) {
        if self.commands.send(Command::Rebuild(session)).is_ok() {
            unsafe { CFRunLoopWakeUp(self.wake.0) };
        }
    }

    /// Stop the subscription but keep the thread parked for the next start.
    pub(crate) fn pause(&self) {
        if self.commands.send(Command::Pause).is_ok() {
            unsafe { CFRunLoopWakeUp(self.wake.0) };
        }
    }
}

/// Start the observer thread. The event sink is called on that thread; it must marshal to main.
pub(crate) fn spawn(sink: EventSink) -> Option<ObserverHandle> {
    let (commands, receiver) = channel();
    let ready: Arc<(Mutex<Option<RunLoopHandle>>, std::sync::Condvar)> =
        Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    let ready_thread = ready.clone();
    let _join = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview".to_string())
        .spawn(move || {
            let run_loop = unsafe { CFRunLoopGetCurrent() };
            *ready_thread.0.lock().unwrap() = Some(RunLoopHandle(run_loop));
            ready_thread.1.notify_all();
            ACTIVE.store(true, Ordering::Release);
            let mut context = ObserverContext::new(sink, receiver);
            context.run();
        })
        .ok()?;
    let (lock, cvar) = &*ready;
    let mut slot = lock.lock().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while slot.is_none() && Instant::now() < deadline {
        let (next, timeout) = cvar.wait_timeout(slot, Duration::from_millis(50)).unwrap();
        slot = next;
        if timeout.timed_out() {
            break;
        }
    }
    let wake = (*slot)?;
    Some(ObserverHandle { commands, wake })
}

struct ObserverContext {
    sink: EventSink,
    receiver: Receiver<Command>,
    run_loop: *mut std::ffi::c_void,
    session: u64,
    dock_pid: Option<i32>,
    observer: Option<AxObserverRef>,
    list: Option<AXUIElementRef>,
    notification: Option<*const std::ffi::c_void>,
    last_poll: Instant,
    last_health: Instant,
    /// Parked by `Pause`: no subscription, no polling, no health rebuild until `Rebuild`.
    paused: bool,
    last_signature: Option<(u8, i32, i8, bool)>,
    /// Declared here so the thread that owns the loop also owns the notification name it removes.
    selection_notification: *const std::ffi::c_void,
}

unsafe impl Send for ObserverContext {}

impl ObserverContext {
    fn new(sink: EventSink, receiver: Receiver<Command>) -> Self {
        Self {
            sink,
            receiver,
            run_loop: std::ptr::null_mut(),
            session: 0,
            dock_pid: None,
            observer: None,
            list: None,
            notification: None,
            last_poll: Instant::now(),
            last_health: Instant::now(),
            paused: false,
            last_signature: None,
            selection_notification: cf_string_new(SELECTION_CHANGED),
        }
    }

    fn run(&mut self) {
        self.run_loop = unsafe { CFRunLoopGetCurrent() };
        self.setup(1);
        loop {
            loop {
                match self.receiver.try_recv() {
                    Ok(Command::Rebuild(session)) => {
                        self.teardown();
                        self.setup(session);
                    }
                    Ok(Command::Pause) => {
                        self.teardown();
                        self.paused = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.teardown();
                        unsafe { CFRelease(self.selection_notification) };
                        return;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                }
            }
            if self.paused {
                // Parked: block until the next command. A health check or poll here would see no
                // subscription and register a new one, leaving the feature observing the Dock after
                // the user turned it off.
                match self.receiver.recv() {
                    Ok(Command::Rebuild(session)) => {
                        self.paused = false;
                        self.setup(session);
                    }
                    Ok(Command::Pause) => {}
                    Err(_) => {
                        unsafe { CFRelease(self.selection_notification) };
                        return;
                    }
                }
                self.last_health = Instant::now();
                self.last_poll = Instant::now();
                continue;
            }
            // Serve the run loop: AX notifications dispatch from here.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 1) };
            let now = Instant::now();
            if now.duration_since(self.last_health) >= HEALTH_INTERVAL {
                self.last_health = now;
                self.health_check();
            }
            if now.duration_since(self.last_poll) >= POLL_INTERVAL {
                self.last_poll = now;
                self.emit(false);
                self.refresh_snapshot();
            }
        }
    }

    fn setup(&mut self, session: u64) {
        self.session = session;
        let Some(dock_pid) = (unsafe { dock_pid() }) else {
            log_debug!("[dock-preview] Dock process not found");
            (self.sink)(DockEvent {
                session,
                selection: Selection::None,
                list_frame: None,
                screen_frame: None,
                revealed: false,
            });
            return;
        };
        self.dock_pid = Some(dock_pid);
        unsafe {
            let app = AXUIElementCreateApplication(dock_pid);
            if app.is_null() {
                return;
            }
            let Some(children) = copy_attribute(app, "AXChildren") else {
                CFRelease(app);
                return;
            };
            let count = CFArrayGetCount(children);
            let mut list: Option<AXUIElementRef> = None;
            for index in 0..count {
                let candidate = CFArrayGetValueAtIndex(children, index) as AXUIElementRef;
                if candidate.is_null() {
                    continue;
                }
                if read_string(candidate, "AXRole").as_deref() == Some("AXList") {
                    list = Some(candidate);
                    break;
                }
            }
            let Some(list) = list else {
                CFRelease(children);
                CFRelease(app);
                return;
            };
            // The array is the element's only owner until this retain: the observer and every
            // later attribute read use the list element after the array itself is released.
            CFRetain(list);
            CFRelease(children);
            let mut observer: AxObserverRef = std::ptr::null_mut();
            if AXObserverCreate(dock_pid, selection_changed, &mut observer) != 0
                || observer.is_null()
            {
                CFRelease(list);
                CFRelease(app);
                return;
            }
            let notify = self.selection_notification;
            let refcon = self as *mut ObserverContext as *mut std::ffi::c_void;
            if AXObserverAddNotification(observer, list, notify, refcon) != 0 {
                CFRelease(observer);
                CFRelease(list);
                CFRelease(app);
                return;
            }
            let source = AXObserverGetRunLoopSource(observer);
            CFRunLoopAddSource(self.run_loop, source, kCFRunLoopDefaultMode);
            self.list = Some(list);
            self.observer = Some(observer);
            self.notification = Some(notify);
            CFRelease(app);
        }
        self.emit(true);
        self.refresh_snapshot();
    }

    fn teardown(&mut self) {
        unsafe {
            if let (Some(observer), Some(list), Some(note)) =
                (self.observer, self.list, self.notification)
            {
                let source = AXObserverGetRunLoopSource(observer);
                CFRunLoopRemoveSource(self.run_loop, source, kCFRunLoopDefaultMode);
                AXObserverRemoveNotification(observer, list, note);
                CFRelease(list);
                CFRelease(observer);
            }
        }
        self.observer = None;
        self.list = None;
        self.notification = None;
        self.dock_pid = None;
        self.last_signature = None;
        (self.sink)(DockEvent {
            session: self.session,
            selection: Selection::None,
            list_frame: None,
            screen_frame: None,
            revealed: false,
        });
    }

    fn health_check(&mut self) {
        let Some(current_pid) = (unsafe { dock_pid() }) else {
            self.teardown();
            self.setup(self.session);
            return;
        };
        let valid = match (self.dock_pid, self.list) {
            (Some(pid), Some(list)) if pid == current_pid => unsafe {
                read_string(list, "AXRole").as_deref() == Some("AXList")
            },
            _ => false,
        };
        if !valid {
            log_debug!("[dock-preview] Dock subscription stale; rebuilding");
            self.teardown();
            self.setup(self.session);
        }
    }

    /// Read the current selection and publish it. `force` bypasses the change signature (a real
    /// notification may carry a new tile rectangle for the same tile, which the panel needs for
    /// anchoring after Dock magnification).
    fn emit(&mut self, force: bool) {
        let Some(list) = self.list else {
            return;
        };
        let Some(list_frame) = (unsafe { element_frame(list) }) else {
            return;
        };
        let screen = unsafe { display_containing(list_frame) };
        let revealed = screen
            .map(|s| dock_revealed(list_frame, s))
            .unwrap_or(false);
        let edge = screen.and_then(|s| dock_edge_from_frames(list_frame, s));
        let selection = unsafe {
            match read_selected_tile(list, edge) {
                Some(selection) => selection,
                None => Selection::None,
            }
        };
        let signature = signature_of(&selection, revealed);
        if !force && self.last_signature == Some(signature) {
            return;
        }
        self.last_signature = Some(signature);
        (self.sink)(DockEvent {
            session: self.session,
            selection,
            list_frame: Some(list_frame),
            screen_frame: screen,
            revealed,
        });
    }

    /// Publish the Dock's tile list for `--e2e-state`. Only built when the state channel is on:
    /// reading every tile's URL costs one Accessibility round trip per tile.
    fn refresh_snapshot(&self) {
        if !crate::e2e_state::is_enabled() {
            return;
        }
        let Some(list) = self.list else {
            return;
        };
        let Some(list_frame) = (unsafe { element_frame(list) }) else {
            return;
        };
        let Some(screen) = (unsafe { display_containing(list_frame) }) else {
            return;
        };
        let mut snapshot = DockSnapshot {
            list: Some(list_frame),
            screen: Some(screen),
            revealed: dock_revealed(list_frame, screen),
            edge: dock_edge_from_frames(list_frame, screen),
            items: Vec::new(),
        };
        unsafe {
            if let Some(children) = copy_attribute(list, "AXChildren") {
                let count = CFArrayGetCount(children);
                for index in 0..count {
                    let child = CFArrayGetValueAtIndex(children, index) as AXUIElementRef;
                    if child.is_null() {
                        continue;
                    }
                    if read_string(child, "AXSubrole").as_deref() != Some("AXApplicationDockItem") {
                        continue;
                    }
                    let Some(bundle) = tile_bundle(child) else {
                        continue;
                    };
                    let Some(rect) = element_frame(child) else {
                        continue;
                    };
                    snapshot.items.push((bundle, rect));
                }
                CFRelease(children);
            }
        }
        *SNAPSHOT.lock().unwrap() = snapshot;
    }
}

unsafe extern "C" fn selection_changed(
    _observer: AxObserverRef,
    _element: *const std::ffi::c_void,
    _notification: *const std::ffi::c_void,
    refcon: *mut std::ffi::c_void,
) {
    crate::callback_guard::void("dock_preview_selection_changed", || {
        if refcon.is_null() {
            return;
        }
        let context = &mut *(refcon as *mut ObserverContext);
        context.emit(true);
    });
}

/// The Dock process's PID.
unsafe fn dock_pid() -> Option<i32> {
    let bundle = make_nsstring(DOCK_BUNDLE_ID);
    let apps: *mut AnyObject = msg_send![
        class!(NSRunningApplication),
        runningApplicationsWithBundleIdentifier: bundle
    ];
    release_obj(bundle);
    if apps.is_null() {
        return None;
    }
    let count: usize = msg_send![apps, count];
    if count == 0 {
        return None;
    }
    let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
    if app.is_null() {
        None
    } else {
        Some(msg_send![app, processIdentifier])
    }
}

/// One attribute, +1 (caller releases).
unsafe fn copy_attribute(element: AXUIElementRef, name: &str) -> Option<*const std::ffi::c_void> {
    if element.is_null() {
        return None;
    }
    let key = cf_string_new(name);
    let mut value: *const std::ffi::c_void = std::ptr::null();
    let error = AXUIElementCopyAttributeValue(element, key, &mut value);
    CFRelease(key);
    (error == 0 && !value.is_null()).then_some(value)
}

/// A string attribute's value.
unsafe fn read_string(element: AXUIElementRef, name: &str) -> Option<String> {
    let value = copy_attribute(element, name)?;
    let is_string = CFGetTypeID(value) == crate::ffi::CFStringGetTypeID();
    let text = if is_string {
        cf_string_value(value)
    } else {
        None
    };
    CFRelease(value);
    text
}

unsafe fn cf_string_value(value: *const std::ffi::c_void) -> Option<String> {
    let length = crate::ffi::CFStringGetLength(value);
    if length < 0 {
        return None;
    }
    let capacity =
        crate::ffi::CFStringGetMaximumSizeForEncoding(length, CF_STRING_ENCODING_UTF8) + 1;
    let mut buffer = vec![0u8; capacity.max(1) as usize];
    let ok = crate::ffi::CFStringGetCString(
        value,
        buffer.as_mut_ptr() as *mut i8,
        buffer.len() as isize,
        CF_STRING_ENCODING_UTF8,
    );
    if !ok {
        return None;
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// The element's frame in top-left global coordinates.
unsafe fn element_frame(element: AXUIElementRef) -> Option<Rect> {
    let position = copy_attribute(element, "AXPosition")?;
    let size = copy_attribute(element, "AXSize")?;
    let mut point = CgPoint { x: 0.0, y: 0.0 };
    let mut dimensions = CgSize { w: 0.0, h: 0.0 };
    let point_ok = AXValueGetValue(
        position,
        K_AX_VALUE_CG_POINT,
        (&mut point as *mut CgPoint).cast(),
    );
    let size_ok = AXValueGetValue(
        size,
        K_AX_VALUE_CG_SIZE,
        (&mut dimensions as *mut CgSize).cast(),
    );
    CFRelease(position);
    CFRelease(size);
    (point_ok && size_ok && dimensions.w > 0.0 && dimensions.h > 0.0).then_some(Rect::new(
        point.x,
        point.y,
        dimensions.w,
        dimensions.h,
    ))
}

/// The active display's bounds in top-left coordinates (display-local origin is global here, so
/// the display with the largest origin offset still reports its own rectangle in the same space).
unsafe fn displays() -> Vec<(u32, crate::ffi::CGRect)> {
    let mut count: u32 = 0;
    if crate::ffi::CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count) != 0 || count == 0 {
        return Vec::new();
    }
    let mut ids = vec![0u32; count as usize];
    let mut actual = 0u32;
    if crate::ffi::CGGetActiveDisplayList(count, ids.as_mut_ptr(), &mut actual) != 0 {
        return Vec::new();
    }
    ids.truncate(actual as usize);
    ids.into_iter()
        .map(|id| (id, crate::ffi::CGDisplayBounds(id)))
        .collect()
}

/// The display a top-left point sits on.
unsafe fn display_containing(frame: Rect) -> Option<Rect> {
    let center = (frame.x + frame.w / 2.0, frame.y + frame.h / 2.0);
    displays()
        .into_iter()
        .map(|(_, bounds)| Rect::new(bounds.x, bounds.y, bounds.w, bounds.h))
        .find(|rect| rect.contains(center))
}

/// Read the hovered Dock tile.
unsafe fn read_selected_tile(list: AXUIElementRef, edge: Option<DockEdge>) -> Option<Selection> {
    let selected = copy_attribute(list, "AXSelectedChildren")?;
    let count = CFArrayGetCount(selected);
    if count <= 0 {
        CFRelease(selected);
        return Some(Selection::None);
    }
    let tile = CFArrayGetValueAtIndex(selected, 0) as AXUIElementRef;
    let result = if tile.is_null() {
        Selection::None
    } else if read_string(tile, "AXSubrole").as_deref() != Some("AXApplicationDockItem") {
        Selection::NonApp
    } else {
        match (tile_bundle(tile), element_frame(tile), edge) {
            (Some(bundle), Some(icon), Some(edge)) => {
                match running_pid_for_bundle(&bundle) {
                    Some(pid) => Selection::App {
                        pid,
                        bundle,
                        title: read_string(tile, "AXTitle").unwrap_or_default(),
                        icon,
                        edge,
                    },
                    // A Dock tile for an app that is not running has nothing to preview.
                    None => Selection::None,
                }
            }
            _ => Selection::NonApp,
        }
    };
    CFRelease(selected);
    Some(result)
}

/// The bundle identifier behind an application Dock tile.
unsafe fn tile_bundle(tile: AXUIElementRef) -> Option<String> {
    let url = copy_attribute(tile, "AXURL")?;
    let path: *mut AnyObject = msg_send![url as *mut AnyObject, path];
    let path = if path.is_null() {
        None
    } else {
        Some(nsstring_to_rust(path))
    };
    CFRelease(url);
    let path = path?;
    if path.is_empty() {
        return None;
    }
    let ns_path = make_nsstring(&path);
    let bundle: *mut AnyObject = msg_send![class!(NSBundle), bundleWithPath: ns_path];
    release_obj(ns_path);
    if bundle.is_null() {
        return None;
    }
    let identifier: *mut AnyObject = msg_send![bundle, bundleIdentifier];
    if identifier.is_null() {
        None
    } else {
        Some(nsstring_to_rust(identifier))
    }
}

unsafe fn running_pid_for_bundle(bundle: &str) -> Option<i32> {
    let ns_bundle = make_nsstring(bundle);
    let apps: *mut AnyObject = msg_send![
        class!(NSRunningApplication),
        runningApplicationsWithBundleIdentifier: ns_bundle
    ];
    release_obj(ns_bundle);
    if apps.is_null() {
        return None;
    }
    let count: usize = msg_send![apps, count];
    if count == 0 {
        return None;
    }
    let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
    if app.is_null() {
        None
    } else {
        Some(msg_send![app, processIdentifier])
    }
}

/// A cheap change signature: what kind of tile is hovered, which app, on which edge, whether the
/// Dock is revealed. Tile rectangles change with magnification and are not part of it.
fn signature_of(selection: &Selection, revealed: bool) -> (u8, i32, i8, bool) {
    match selection {
        Selection::App { pid, edge, .. } => (
            1,
            *pid,
            match edge {
                DockEdge::Bottom => 0,
                DockEdge::Top => 1,
                DockEdge::Left => 2,
                DockEdge::Right => 3,
            },
            revealed,
        ),
        Selection::NonApp => (2, 0, 0, revealed),
        Selection::None => (0, 0, 0, revealed),
    }
}
