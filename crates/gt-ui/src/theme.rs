pub use gt_render::D2D1_COLOR_F as Color;

pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color { r, g, b, a }
}

pub const fn hex(v: u32, a: f32) -> Color {
    rgba(((v >> 16) & 0xFF) as f32 / 255.0, ((v >> 8) & 0xFF) as f32 / 255.0, (v & 0xFF) as f32 / 255.0, a)
}

/// Dark HUD palette in the spirit of Quick Look's panel, with Windows 11 accents.
#[derive(Clone, Debug)]
pub struct Theme {
    pub window_bg: Color,
    pub title_bg: Color,
    pub panel_bg: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub accent: Color,
    pub hover: Color,
    pub pressed: Color,
    pub selection: Color,
    pub separator: Color,
    pub close_hover: Color,
    pub scrollbar: Color,
    pub row_alt: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            window_bg: rgba(0.11, 0.11, 0.115, 0.94),
            title_bg: rgba(0.16, 0.16, 0.165, 0.97),
            panel_bg: rgba(0.14, 0.14, 0.145, 0.98),
            text: rgba(0.94, 0.94, 0.95, 1.0),
            text2: rgba(0.70, 0.70, 0.72, 1.0),
            text3: rgba(0.50, 0.50, 0.53, 1.0),
            accent: hex(0x4CC2FF, 1.0),
            hover: rgba(1.0, 1.0, 1.0, 0.08),
            pressed: rgba(1.0, 1.0, 1.0, 0.04),
            selection: hex(0x4CC2FF, 0.35),
            separator: rgba(1.0, 1.0, 1.0, 0.08),
            close_hover: hex(0xC42B1C, 1.0),
            scrollbar: rgba(1.0, 1.0, 1.0, 0.35),
            row_alt: rgba(1.0, 1.0, 1.0, 0.025),
        }
    }
}
