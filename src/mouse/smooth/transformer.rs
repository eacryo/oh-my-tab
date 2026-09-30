// MIT License
// Copyright (c) 2021-2024 LinearMouse

//! CGEvent delivery around the pure smooth-scroll engine. All methods run on the mouse tap's
//! dedicated run-loop thread, so the timer posts without crossing the fail-open poster queue.

use super::delivery::{integer_delta, phase_fields, SubpixelAccumulator, POINTS_PER_INPUT_LINE};
use super::engine::{Axis, InputKind, Phase, SmoothEngine};
use super::presets::SmoothSettings;
use crate::event_tap::{
    self, CFRunLoopGetCurrent, CFRunLoopTimerRef, CGEventCreateScrollWheelEvent2, CGEventFlags,
    CGEventGetDoubleValueField, CGEventGetFlags, CGEventGetIntegerValueField, CGEventPost,
    CGEventSetDoubleValueField, CGEventSetFlags, CGEventSetIntegerValueField,
    K_CG_EVENT_SOURCE_USER_DATA, K_CG_SCROLL_EVENT_UNIT_PIXEL,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE,
    K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE, K_CG_SESSION_EVENT_TAP, SYNTHETIC_MARKER,
};
use std::ffi::c_void;
use std::time::Instant;

const TICK_INTERVAL: f64 = 1.0 / 120.0;

pub(crate) struct SmoothTransformer {
    engine: SmoothEngine,
    settings: Option<SmoothSettings>,
    timer: CFRunLoopTimerRef,
    accumulator: SubpixelAccumulator,
    last_flags: CGEventFlags,
    origin: Instant,
}

impl SmoothTransformer {
    pub(crate) fn new() -> Self {
        Self {
            engine: SmoothEngine::new(SmoothSettings::default()),
            settings: None,
            timer: std::ptr::null_mut(),
            accumulator: SubpixelAccumulator::default(),
            last_flags: 0,
            origin: Instant::now(),
        }
    }

    /// Feed an already reversed discrete-wheel delta. Returns false only if timer setup fails,
    /// allowing the caller to fail open and pass the original event through.
    pub(crate) unsafe fn feed(
        &mut self,
        settings: SmoothSettings,
        delta_x_lines: f64,
        delta_y_lines: f64,
        flags: CGEventFlags,
    ) -> bool {
        if !settings.enabled {
            return false;
        }
        if self.settings != Some(settings) {
            self.stop_timer();
            self.engine = SmoothEngine::new(settings);
            self.accumulator = SubpixelAccumulator::default();
            self.settings = Some(settings);
        }
        if self.timer.is_null() && !self.start_timer() {
            self.settings = None;
            return false;
        }

        let dx = delta_x_lines * POINTS_PER_INPUT_LINE;
        let dy = delta_y_lines * POINTS_PER_INPUT_LINE;
        if dx != 0.0 && dy == 0.0 {
            self.engine.reset_other_axis(Axis::Horizontal);
        } else if dy != 0.0 && dx == 0.0 {
            self.engine.reset_other_axis(Axis::Vertical);
        }
        self.last_flags = flags;
        let now = self.origin.elapsed().as_secs_f64();
        self.engine.feed(dx, dy, now, InputKind::Wheel);
        true
    }

    unsafe fn start_timer(&mut self) -> bool {
        let context = event_tap::CFRunLoopTimerContext {
            version: 0,
            info: self as *mut Self as *mut c_void,
            retain: None,
            release: None,
            copy_description: None,
        };
        let timer = event_tap::CFRunLoopTimerCreate(
            std::ptr::null_mut(),
            0.0,
            TICK_INTERVAL,
            0,
            0,
            Some(smooth_timer_callback),
            &context as *const event_tap::CFRunLoopTimerContext as *mut c_void,
        );
        if timer.is_null() {
            return false;
        }
        event_tap::CFRunLoopAddTimer(
            CFRunLoopGetCurrent(),
            timer,
            event_tap::kCFRunLoopDefaultMode,
        );
        self.timer = timer;
        true
    }

    unsafe fn stop_timer(&mut self) {
        if !self.timer.is_null() {
            event_tap::CFRunLoopTimerInvalidate(self.timer);
            crate::ffi::CFRelease(self.timer as *const c_void);
            self.timer = std::ptr::null_mut();
        }
    }

    pub(crate) unsafe fn shutdown(&mut self) {
        self.stop_timer();
        self.settings = None;
        self.accumulator = SubpixelAccumulator::default();
        self.engine = SmoothEngine::new(SmoothSettings::default());
    }

    unsafe fn tick(&mut self) {
        if crate::mouse::event_tap::mouse_tap_stopping() || !crate::input_monitor::taps_allowed() {
            self.shutdown();
            return;
        }
        crate::e2e_state::smooth_scroll_tick();
        if let Some(emission) = self.engine.advance(self.origin.elapsed().as_secs_f64()) {
            crate::e2e_state::smooth_scroll_phase(emission.phase);
            self.post_emission(emission.phase, emission.delta_x, emission.delta_y);
        }
        if !self.engine.is_running() {
            self.stop_timer();
            crate::schedule_smooth_scroll_e2e_record();
        }
    }

    unsafe fn post_emission(&mut self, phase: Phase, dx: f64, dy: f64) {
        let delivered = self.accumulator.convert(dx, dy, phase);
        let synthetic = CGEventCreateScrollWheelEvent2(
            std::ptr::null(),
            K_CG_SCROLL_EVENT_UNIT_PIXEL,
            2,
            0,
            0,
            0,
        );
        if synthetic.is_null() {
            crate::log_info!("[mouse] failed to synthesize smooth scroll event");
            return;
        }
        CGEventSetIntegerValueField(synthetic, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
        let fields = phase_fields(phase);
        // CoreGraphics couples the legacy line fields to the pixel/fixed-point values while they
        // are being set. Keep LinearMouse's setter order so later fields preserve these values.
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
            integer_delta(delivered.point_y as f64),
        );
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
            integer_delta(delivered.point_x as f64),
        );
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
            delivered.point_y,
        );
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
            delivered.point_x,
        );
        CGEventSetDoubleValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1,
            delivered.fixed_y,
        );
        CGEventSetDoubleValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
            delivered.fixed_x,
        );
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE,
            fields.scroll_phase,
        );
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE,
            fields.momentum_phase,
        );
        CGEventSetFlags(synthetic, self.last_flags);
        CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
        CGEventPost(K_CG_SESSION_EVENT_TAP, synthetic);
        crate::ffi::CFRelease(synthetic as *const c_void);
    }
}

unsafe extern "C" fn smooth_timer_callback(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    if !info.is_null() {
        (&mut *(info as *mut SmoothTransformer)).tick();
    }
}

/// A1 smoke assertion for the SDK field IDs and the values written by the smooth event path.
pub(crate) unsafe fn smoke_event_fields() -> bool {
    let event =
        CGEventCreateScrollWheelEvent2(std::ptr::null(), K_CG_SCROLL_EVENT_UNIT_PIXEL, 2, 0, 0, 0);
    if event.is_null() {
        return false;
    }
    let flags = 0x0010_0000;
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, -2);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, 1);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1, -35);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2, 17);
    CGEventSetDoubleValueField(event, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1, -1.25);
    CGEventSetDoubleValueField(event, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2, 2.5);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE, 2);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE, 0);
    CGEventSetFlags(event, flags);
    CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);

    let fixed_y = CGEventGetDoubleValueField(event, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1);
    let fixed_x = CGEventGetDoubleValueField(event, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2);
    let point_y = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
    let point_x = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
    let scroll_phase = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE);
    let momentum_phase = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE);
    let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
    let marker = CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA);
    let observed_flags = CGEventGetFlags(event);
    let integer_y = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
    let integer_x = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
    let ok = (fixed_y + 1.25).abs() < 1e-9
        && (fixed_x - 2.5).abs() < 1e-9
        && point_y == -35
        && point_x == 17
        && scroll_phase == 2
        && momentum_phase == 0
        && continuous == 1
        && marker == SYNTHETIC_MARKER
        && observed_flags == flags
        && integer_y == -2
        && integer_x == 1;
    if !ok {
        eprintln!(
            "[smoke-smooth-scroll-event] fields fixed=({fixed_y},{fixed_x}) point=({point_y},{point_x}) integer=({integer_y},{integer_x}) phase=({scroll_phase},{momentum_phase}) continuous={continuous} flags={observed_flags:#x} marker={marker:#x}"
        );
    }
    crate::ffi::CFRelease(event as *const c_void);
    ok
}
