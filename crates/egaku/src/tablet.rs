//! Graphics tablet input.
//!
//! winit only reports pen force where the OS presents the pen as a touch, so
//! egaku asks the platform directly:
//!
//! * Linux (Wayland) and Windows: `octotablet`, over Wayland's tablet
//!   protocol and Windows Ink.
//! * macOS: AppKit puts pen pressure on ordinary mouse events; a local event
//!   monitor reads it (and the eraser end) without disturbing anything.
//!
//! That gives true pressure on Wacom, XP-Pen, Huion and similar pens, and
//! tells us when the eraser end is in use. Elsewhere (X11, or when no tablet
//! is found) pressure falls back to touch force or full strength.

/// The latest pen state.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct PenState {
    /// Pressure 0..=1 while the pen touches the surface.
    pub pressure: Option<f32>,
    /// The tool in range is an eraser (the back of the pen).
    pub eraser: bool,
}

#[cfg(all(feature = "tablet", not(target_os = "macos")))]
mod imp {
    use super::PenState;

    pub struct Backend {
        mgr: octotablet::Manager,
        state: PenState,
        down: bool,
    }

    impl Backend {
        pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Backend> {
            // Safety: the window and display outlive the app (eframe drops
            // the app before tearing down its window).
            let mgr = unsafe { octotablet::Builder::new().build_raw(cc) }.ok()?;
            Some(Backend {
                mgr,
                state: PenState::default(),
                down: false,
            })
        }

        // On Windows pumping cannot fail, which makes the `else` unreachable.
        #[allow(
            unreachable_patterns,
            irrefutable_let_patterns,
            clippy::needless_return
        )]
        pub fn poll(&mut self) -> PenState {
            use octotablet::events::{Event, ToolEvent};
            use octotablet::tool::Type;
            let Ok(events) = self.mgr.pump() else {
                return self.state;
            };
            for ev in events {
                let Event::Tool { tool, event } = ev else {
                    continue;
                };
                match event {
                    ToolEvent::In { .. } => {
                        self.state.eraser = tool.tool_type == Some(Type::Eraser);
                    }
                    ToolEvent::Down => self.down = true,
                    ToolEvent::Up => {
                        self.down = false;
                        self.state.pressure = None;
                    }
                    ToolEvent::Out => {
                        self.down = false;
                        self.state = PenState::default();
                    }
                    ToolEvent::Pose(p) if self.down => {
                        self.state.pressure = Some(p.pressure.get().unwrap_or(1.0).clamp(0.0, 1.0));
                    }
                    _ => {}
                }
            }
            self.state
        }
    }
}

#[cfg(all(feature = "tablet", target_os = "macos"))]
mod imp {
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use block2::RcBlock;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventSubtype, NSEventType, NSPointingDeviceType};

    use super::PenState;

    /// Pressure as f32 bits; NaN while the pen is up.
    static PRESSURE: AtomicU32 = AtomicU32::new(0x7fc0_0000);
    static ERASER: AtomicBool = AtomicBool::new(false);

    pub struct Backend;

    impl Backend {
        pub fn new(_: &eframe::CreationContext<'_>) -> Option<Backend> {
            let mask = NSEventMask::LeftMouseDown
                | NSEventMask::LeftMouseDragged
                | NSEventMask::LeftMouseUp
                | NSEventMask::TabletPoint
                | NSEventMask::TabletProximity;
            let block = RcBlock::new(|ev: NonNull<NSEvent>| -> *mut NSEvent {
                // Safety: AppKit hands us a valid event for the duration of
                // the call; we only read from it and pass it on unchanged.
                let e = unsafe { ev.as_ref() };
                let ty = e.r#type();
                let tablet = ty == NSEventType::TabletPoint
                    || ty == NSEventType::TabletProximity
                    || matches!(
                        e.subtype(),
                        NSEventSubtype::TabletPoint | NSEventSubtype::TabletProximity
                    );
                if ty == NSEventType::TabletProximity
                    || (tablet && e.subtype() == NSEventSubtype::TabletProximity)
                {
                    ERASER.store(
                        e.isEnteringProximity()
                            && e.pointingDeviceType() == NSPointingDeviceType::Eraser,
                        Ordering::Relaxed,
                    );
                } else if ty == NSEventType::LeftMouseUp {
                    PRESSURE.store(f32::NAN.to_bits(), Ordering::Relaxed);
                } else if tablet {
                    PRESSURE.store(e.pressure().clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                }
                ev.as_ptr()
            });
            // Safety: the block returns the event it was given.
            let monitor =
                unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
            // The monitor lives as long as the app.
            std::mem::forget(monitor);
            Some(Backend)
        }

        pub fn poll(&mut self) -> PenState {
            let p = f32::from_bits(PRESSURE.load(Ordering::Relaxed));
            PenState {
                pressure: (!p.is_nan()).then_some(p),
                eraser: ERASER.load(Ordering::Relaxed),
            }
        }
    }
}

pub struct Tablet {
    #[cfg(feature = "tablet")]
    backend: imp::Backend,
}

impl Tablet {
    #[cfg(feature = "tablet")]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Tablet> {
        imp::Backend::new(cc).map(|backend| Tablet { backend })
    }

    #[cfg(not(feature = "tablet"))]
    pub fn new(_: &eframe::CreationContext<'_>) -> Option<Tablet> {
        None
    }

    /// Drains pending tablet events and returns the current pen state.
    pub fn poll(&mut self) -> PenState {
        #[cfg(feature = "tablet")]
        {
            self.backend.poll()
        }
        #[cfg(not(feature = "tablet"))]
        PenState::default()
    }
}
