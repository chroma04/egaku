//! Graphics tablet input.
//!
//! winit only reports pen force where the OS presents the pen as a touch, so
//! egaku talks to the platform tablet API directly through `octotablet`:
//! Wayland's tablet protocol and Windows Ink. That gives true pressure on
//! Wacom, XP-Pen, Huion and similar pens, and tells us when the eraser end
//! is in use. Everywhere else (and when no tablet is found) this is a no-op
//! and pressure falls back to touch force or full strength.

/// The latest pen state.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct PenState {
    /// Pressure 0..=1 while the pen touches the surface.
    pub pressure: Option<f32>,
    /// The tool in range is an eraser (the back of the pen).
    pub eraser: bool,
}

pub struct Tablet {
    #[cfg(feature = "tablet")]
    mgr: octotablet::Manager,
    state: PenState,
    down: bool,
}

impl Tablet {
    #[cfg(feature = "tablet")]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Tablet> {
        // Safety: the window and display outlive the app (eframe drops the
        // app before tearing down its window).
        let mgr = unsafe { octotablet::Builder::new().build_raw(cc) }.ok()?;
        Some(Tablet {
            mgr,
            state: PenState::default(),
            down: false,
        })
    }

    #[cfg(not(feature = "tablet"))]
    pub fn new(_: &eframe::CreationContext<'_>) -> Option<Tablet> {
        None
    }

    /// Drains pending tablet events and returns the current pen state.
    pub fn poll(&mut self) -> PenState {
        #[cfg(feature = "tablet")]
        {
            use octotablet::events::{Event, ToolEvent};
            use octotablet::tool::Type;
            let Ok(events) = self.mgr.pump() else {
                return self.state;
            };
            for ev in events {
                let Event::Tool { tool, event } = ev else { continue };
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
                    ToolEvent::Pose(p) => {
                        if self.down {
                            self.state.pressure = Some(p.pressure.get().unwrap_or(1.0).clamp(0.0, 1.0));
                        }
                    }
                    _ => {}
                }
            }
        }
        self.state
    }
}
