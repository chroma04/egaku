//! Tools and the settings remembered between sessions.

use std::path::PathBuf;

use egaku_core::brush::BrushParams;
use egaku_core::io::Format;
use egaku_core::object::TextAlign;
use egui_phosphor::regular as ph;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    Move,
    Select,
    Brush,
    Eraser,
    Fill,
    Gradient,
    Text,
    Picker,
    Crop,
    Hand,
}

impl Tool {
    /// Tool strip order; `None` marks a gap between groups.
    pub const STRIP: [Option<Tool>; 13] = [
        Some(Tool::Move),
        Some(Tool::Select),
        Some(Tool::Crop),
        None,
        Some(Tool::Brush),
        Some(Tool::Eraser),
        Some(Tool::Fill),
        Some(Tool::Gradient),
        Some(Tool::Text),
        None,
        Some(Tool::Picker),
        Some(Tool::Hand),
        None,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Move => "Move",
            Tool::Select => "Select",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Fill => "Fill",
            Tool::Gradient => "Gradient",
            Tool::Text => "Text",
            Tool::Picker => "Eyedropper",
            Tool::Crop => "Crop",
            Tool::Hand => "Hand",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tool::Move => "V",
            Tool::Select => "M",
            Tool::Brush => "B",
            Tool::Eraser => "E",
            Tool::Fill => "G",
            Tool::Gradient => "Shift+G",
            Tool::Text => "T",
            Tool::Picker => "I",
            Tool::Crop => "C",
            Tool::Hand => "H",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Move => ph::ARROWS_OUT_CARDINAL,
            Tool::Select => ph::SELECTION,
            Tool::Brush => ph::PAINT_BRUSH,
            Tool::Eraser => ph::ERASER,
            Tool::Fill => ph::PAINT_BUCKET,
            Tool::Gradient => ph::GRADIENT,
            Tool::Text => ph::TEXT_T,
            Tool::Picker => ph::EYEDROPPER,
            Tool::Crop => ph::CROP,
            Tool::Hand => ph::HAND,
        }
    }

    /// What the tool does, in a sentence, for tooltips.
    pub fn blurb(self) -> &'static str {
        match self {
            Tool::Move => "Drag to move the layer or selection. Ctrl+T to scale and rotate.",
            Tool::Select => "Drag to select. Shift adds, Alt subtracts.",
            Tool::Brush => "Paint. Shift-click draws a straight line.",
            Tool::Eraser => "Erase to transparency.",
            Tool::Fill => "Fill an area of similar colour.",
            Tool::Gradient => "Drag to draw a gradient from the main to the second colour.",
            Tool::Text => "Click to add text, or click text to edit it.",
            Tool::Picker => "Pick a colour from the canvas. Hold Alt with painting tools.",
            Tool::Crop => "Drag to choose what to keep, then press Enter.",
            Tool::Hand => "Drag to pan. Hold Space with any tool.",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectShape {
    Rect,
    Ellipse,
    Lasso,
    Wand,
}

impl SelectShape {
    pub const ALL: [SelectShape; 4] = [
        SelectShape::Rect,
        SelectShape::Ellipse,
        SelectShape::Lasso,
        SelectShape::Wand,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SelectShape::Rect => "Rectangle",
            SelectShape::Ellipse => "Ellipse",
            SelectShape::Lasso => "Lasso",
            SelectShape::Wand => "Magic wand",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            SelectShape::Rect => ph::SELECTION,
            SelectShape::Ellipse => ph::CIRCLE_DASHED,
            SelectShape::Lasso => ph::LASSO,
            SelectShape::Wand => ph::MAGIC_WAND,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            SelectShape::Rect | SelectShape::Ellipse => "M",
            SelectShape::Lasso => "L",
            SelectShape::Wand => "W",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GradientKind {
    Linear,
    Radial,
}

/// A named starting point for brush settings.
pub struct Preset {
    pub name: &'static str,
    pub params: BrushParams,
}

pub fn presets() -> [Preset; 6] {
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
        Preset {
            name: "Pixel",
            params: BrushParams {
                size: 1.0,
                hardness: 1.0,
                pressure_size: false,
                smoothing: 0.0,
                pixel: true,
                pixel_perfect: true,
                ..d
            },
        },
    ]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TextDefaults {
    pub font: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub align_i: u8,
    pub line_height: f32,
}

impl Default for TextDefaults {
    fn default() -> Self {
        TextDefaults {
            font: String::new(),
            size: 72.0,
            bold: false,
            italic: false,
            align_i: 0,
            line_height: 1.2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub tool: Tool,
    pub select_shape: SelectShape,
    pub select_antialias: bool,
    pub wand_tolerance: u8,
    pub wand_sample_all: bool,
    pub brush: BrushParams,
    pub eraser: BrushParams,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub recent: Vec<[u8; 3]>,
    pub palette: Vec<[u8; 3]>,
    pub fill_tolerance: u8,
    pub fill_grow: u32,
    pub fill_sample_all: bool,
    pub gradient: GradientKind,
    pub gradient_transparent: bool,
    pub text: TextDefaults,
    pub auto_select: bool,
    pub smooth_transform: bool,
    pub new_width: u32,
    pub new_height: u32,
    pub new_paper: Option<[u8; 3]>,
    pub last_dir: Option<PathBuf>,
    pub recent_files: Vec<PathBuf>,
    pub pixel_grid: bool,
    pub grid_size: u32,
    pub export_format: Format,
    pub export_scale: u32,
    pub show_timeline: bool,
    pub onion: bool,
    pub panel_colour: bool,
    pub panel_swatches: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            tool: Tool::Brush,
            select_shape: SelectShape::Rect,
            select_antialias: true,
            wand_tolerance: 32,
            wand_sample_all: true,
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
            palette: default_palette(),
            fill_tolerance: 24,
            fill_grow: 1,
            fill_sample_all: true,
            gradient: GradientKind::Linear,
            gradient_transparent: false,
            text: TextDefaults::default(),
            auto_select: true,
            smooth_transform: true,
            new_width: 3000,
            new_height: 2000,
            new_paper: Some([255, 255, 255]),
            last_dir: None,
            recent_files: vec![],
            pixel_grid: true,
            grid_size: 0,
            export_format: Format::Png,
            export_scale: 1,
            show_timeline: false,
            onion: true,
            panel_colour: true,
            panel_swatches: true,
        }
    }
}

/// A small, balanced starter palette: greys, then hues at two values.
fn default_palette() -> Vec<[u8; 3]> {
    vec![
        [0, 0, 0],
        [64, 64, 68],
        [128, 128, 132],
        [192, 192, 196],
        [255, 255, 255],
        [222, 66, 52],
        [240, 140, 40],
        [246, 206, 70],
        [96, 180, 76],
        [44, 150, 170],
        [54, 102, 210],
        [130, 76, 196],
        [214, 92, 160],
        [134, 84, 54],
        [240, 200, 170],
        [24, 40, 72],
    ]
}

impl Prefs {
    /// Remembers a colour in the recent list (most recent first).
    pub fn remember_color(&mut self, c: [u8; 3]) {
        self.recent.retain(|r| *r != c);
        self.recent.insert(0, c);
        self.recent.truncate(24);
    }

    pub fn remember_file(&mut self, p: PathBuf) {
        self.recent_files.retain(|r| *r != p);
        self.recent_files.insert(0, p);
        self.recent_files.truncate(10);
    }

    pub fn text_align(&self) -> TextAlign {
        match self.text.align_i {
            1 => TextAlign::Center,
            2 => TextAlign::Right,
            _ => TextAlign::Left,
        }
    }
}
