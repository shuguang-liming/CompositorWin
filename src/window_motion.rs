use eframe::egui;

const OPEN_SECONDS: f64 = 0.160;
const CLOSE_SECONDS: f64 = 0.120;

#[derive(Clone, Copy, Debug)]
enum Phase {
    FirstFrame,
    AwaitVisibleFrame(u64),
    Opening { started: f64 },
    Open,
    Closing { started: f64, from_opacity: f32 },
    CloseReady,
    Closed,
}

#[derive(Clone, Copy, Debug)]
struct MotionFrame {
    opacity: f32,
    repaint: bool,
    close_now: bool,
}

/// Short content transitions without changing the native window or its swapchain.
pub struct WindowMotion {
    phase: Phase,
    animations_enabled: bool,
}

impl Default for WindowMotion {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowMotion {
    pub fn new() -> Self {
        Self::with_animations(system_animations_enabled())
    }

    pub fn with_animations(enabled: bool) -> Self {
        Self {
            phase: if enabled {
                Phase::FirstFrame
            } else {
                Phase::Open
            },
            animations_enabled: enabled,
        }
    }

    pub fn animations_enabled(&self) -> bool {
        self.animations_enabled
    }

    pub fn refresh_system_preference(&mut self) {
        self.set_animations_enabled(system_animations_enabled());
    }

    pub fn set_animations_enabled(&mut self, enabled: bool) {
        self.animations_enabled = enabled;
        if !enabled {
            self.phase = match self.phase {
                Phase::FirstFrame | Phase::AwaitVisibleFrame(_) | Phase::Opening { .. } => {
                    Phase::Open
                }
                Phase::Closing { .. } => Phase::CloseReady,
                other => other,
            };
        }
    }

    /// Call only after all save confirmations and active operations have finished.
    pub fn begin_close(&mut self, now: f64) {
        if matches!(
            self.phase,
            Phase::Closing { .. } | Phase::CloseReady | Phase::Closed
        ) {
            return;
        }
        self.phase = if self.animations_enabled {
            Phase::Closing {
                started: now,
                from_opacity: self.opacity(now),
            }
        } else {
            Phase::CloseReady
        };
    }

    pub fn is_closing(&self) -> bool {
        matches!(self.phase, Phase::Closing { .. } | Phase::CloseReady)
    }

    pub fn cancel_close(&mut self) {
        if self.is_closing() {
            self.phase = Phase::Open;
        }
    }

    /// Preserve viewport events so repeated native close requests are still canceled.
    pub fn suppress_input(&self, ctx: &egui::Context) {
        if !self.is_closing() {
            return;
        }
        ctx.input_mut(|input| {
            input.events.clear();
            input.keys_down.clear();
            input.pointer = Default::default();
            input.modifiers = Default::default();
            input.raw_scroll_delta = egui::Vec2::ZERO;
            input.smooth_scroll_delta = egui::Vec2::ZERO;
            input.raw.events.clear();
            input.raw.dropped_files.clear();
            input.raw.hovered_files.clear();
        });
    }

    /// Paint last, after the complete editor UI. Returns true once it is safe to close.
    pub fn paint(&mut self, ctx: &egui::Context) -> bool {
        // Check changes only during these short transitions, never while idle.
        if !matches!(self.phase, Phase::Open | Phase::Closed) {
            self.refresh_system_preference();
        }
        // A minimized window may stop presenting frames, so never wait on its transition.
        if ctx.input(|input| input.viewport().minimized == Some(true)) {
            self.set_animations_enabled(false);
        }
        let frame = self.advance(ctx.input(|input| input.time), ctx.cumulative_frame_nr());
        if frame.opacity > 0.0 {
            let fill = ctx.style().visuals.panel_fill.gamma_multiply(frame.opacity);
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("window-content-transition"),
            ))
            .rect_filled(ctx.viewport_rect(), 0.0, fill);
        }
        if frame.repaint {
            // The existing VSync follows the monitor; animation never adds idle redraws.
            ctx.request_repaint();
        }
        frame.close_now
    }

    fn opacity(&self, now: f64) -> f32 {
        match self.phase {
            Phase::FirstFrame | Phase::AwaitVisibleFrame(_) | Phase::CloseReady | Phase::Closed => {
                1.0
            }
            Phase::Opening { started } => (1.0 - progress(now, started, OPEN_SECONDS)).powi(3),
            Phase::Open => 0.0,
            Phase::Closing {
                started,
                from_opacity,
            } => {
                from_opacity + (1.0 - from_opacity) * progress(now, started, CLOSE_SECONDS).powi(3)
            }
        }
    }

    fn advance(&mut self, now: f64, frame_nr: u64) -> MotionFrame {
        self.phase = match self.phase {
            Phase::FirstFrame => Phase::AwaitVisibleFrame(frame_nr),
            // A discarded layout pass is not a displayed frame.
            Phase::AwaitVisibleFrame(first_frame) if frame_nr > first_frame => {
                Phase::Opening { started: now }
            }
            Phase::Opening { started } if now - started >= OPEN_SECONDS => Phase::Open,
            Phase::Closing { started, .. } if now - started >= CLOSE_SECONDS => Phase::CloseReady,
            other => other,
        };
        let opacity = self.opacity(now);
        let close_now = matches!(self.phase, Phase::CloseReady);
        if close_now {
            self.phase = Phase::Closed;
        }
        MotionFrame {
            opacity,
            repaint: !matches!(self.phase, Phase::Open | Phase::Closed),
            close_now,
        }
    }
}

fn progress(now: f64, started: f64, duration: f64) -> f32 {
    ((now - started) / duration).clamp(0.0, 1.0) as f32
}

pub fn system_animations_enabled() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn SystemParametersInfoW(
                action: u32,
                parameter: u32,
                value: *mut std::ffi::c_void,
                flags: u32,
            ) -> i32;
        }
        const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
        let mut enabled = 0_i32;
        // SPI copies a BOOL into this local buffer and retains no pointer.
        let succeeded = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut enabled as *mut i32).cast(),
                0,
            )
        };
        // Reduced motion is also the fallback if the preference cannot be read.
        succeeded != 0 && enabled != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
#[path = "window_motion_tests.rs"]
mod tests;
