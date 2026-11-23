//! Settings remembered between sessions.

use std::path::PathBuf;

use egaku_core::brush::BrushParams;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    Brush,
    Eraser,
    Fill,
    Picker,
    Move,
    Hand,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Brush,
        Tool::Eraser,
        Tool::Fill,
        Tool::Picker,
        Tool::Move,
        Tool::Hand,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Fill => "Fill",
            Tool::Picker => "Eyedropper",
            Tool::Move => "Move",
            Tool::Hand => "Hand",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tool::Brush => "B",
            Tool::Eraser => "E",
            Tool::Fill => "G",
            Tool::Picker => "I",
            Tool::Move => "V",
            Tool::Hand => "H",
        }
    }

    pub fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            Tool::Brush => Icon::Brush,
            Tool::Eraser => Icon::Eraser,
            Tool::Fill => Icon::Fill,
            Tool::Picker => Icon::Picker,
            Tool::Move => Icon::Move,
            Tool::Hand => Icon::Hand,
        }
    }
}

/// A named starting point for brush settings.
pub struct Preset {
    pub name: &'static str,
    pub params: BrushParams,
}

pub fn presets() -> [Preset; 5] {
    let d = BrushParams::default();
    [
        Preset {
            name: "Pen",
            params: BrushParams {
                size: 6.0,
                hardness: 0.9,
                flow: 1.0,
                smoothing: 0.35,
                ..d
            },
        },
        Preset {
            name: "Ink",
            params: BrushParams {
                size: 10.0,
                hardness: 0.95,
                min_size: 0.05,
                smoothing: 0.55,
                ..d
            },
        },
        Preset {
            name: "Pencil",
            params: BrushParams {
                size: 3.0,
                hardness: 0.6,
                opacity: 0.85,
                pressure_opacity: true,
                smoothing: 0.1,
                ..d
            },
        },
        Preset {
            name: "Marker",
            params: BrushParams {
                size: 28.0,
                hardness: 0.8,
                opacity: 0.6,
                pressure_size: false,
                smoothing: 0.2,
                ..d
            },
        },
        Preset {
            name: "Airbrush",
            params: BrushParams {
                size: 160.0,
                hardness: 0.0,
                flow: 0.06,
                pressure_size: false,
                pressure_opacity: true,
                smoothing: 0.2,
                ..d
            },
        },
    ]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub tool: Tool,
    pub brush: BrushParams,
    pub eraser: BrushParams,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub recent: Vec<[u8; 3]>,
    pub fill_tolerance: u8,
    pub fill_grow: u32,
    pub fill_sample_all: bool,
    pub new_width: u32,
    pub new_height: u32,
    pub new_paper: Option<[u8; 3]>,
    pub last_dir: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            tool: Tool::Brush,
            brush: presets()[0].params,
            eraser: BrushParams {
                size: 40.0,
                hardness: 0.7,
                pressure_size: false,
                smoothing: 0.1,
                ..Default::default()
            },
            fg: [24, 24, 28],
            bg: [255, 255, 255],
            recent: vec![],
            fill_tolerance: 24,
            fill_grow: 1,
            fill_sample_all: true,
            new_width: 3000,
            new_height: 2000,
            new_paper: Some([255, 255, 255]),
            last_dir: None,
        }
    }
}

impl Prefs {
    /// Remembers a colour in the recent list (most recent first).
    pub fn remember_color(&mut self, c: [u8; 3]) {
        self.recent.retain(|r| *r != c);
        self.recent.insert(0, c);
        self.recent.truncate(16);
    }
}
