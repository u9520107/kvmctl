use std::convert::TryInto;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyIcon, GetSystemMetrics, HICON, SM_CXSMICON,
};

const ICON_VERSION: u32 = 0x0003_0000;
const CONTRAST: Color = Color(32, 35, 38, 255);
const WHITE: Color = Color(245, 247, 248, 255);
const GREEN: Color = Color(69, 214, 126, 255);
const AMBER: Color = Color(244, 168, 54, 255);
const VIOLET: Color = Color(174, 120, 255, 255);
const RED: Color = Color(239, 84, 84, 255);

#[derive(Clone, Copy)]
pub(crate) enum TrayState {
    NoTarget,
    Connecting,
    ConnectedLocal,
    OwnershipPending,
    Recovering,
    Unavailable,
    OwnershipLost,
}

const STATES: [TrayState; 7] = [
    TrayState::NoTarget,
    TrayState::Connecting,
    TrayState::ConnectedLocal,
    TrayState::OwnershipPending,
    TrayState::Recovering,
    TrayState::Unavailable,
    TrayState::OwnershipLost,
];

pub(crate) struct TrayIcon {
    pub(crate) window: HWND,
    icons: [HICON; STATES.len()],
}

impl TrayIcon {
    pub(crate) fn new(window: HWND) -> Result<Self, String> {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
        let mut icons = Vec::with_capacity(STATES.len());
        for state in STATES {
            match create_icon(size, state) {
                Ok(icon) => icons.push(icon),
                Err(error) => {
                    destroy_icons(&icons);
                    return Err(error);
                }
            }
        }
        let icons: [HICON; STATES.len()] = icons
            .try_into()
            .map_err(|_| "could not build tray icon set".to_string())?;
        Ok(Self { window, icons })
    }

    pub(crate) fn icon(&self, state: TrayState) -> HICON {
        self.icons[state.index()]
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        destroy_icons(&self.icons);
    }
}

impl TrayState {
    fn index(self) -> usize {
        match self {
            Self::NoTarget => 0,
            Self::Connecting => 1,
            Self::ConnectedLocal => 2,
            Self::OwnershipPending => 3,
            Self::Recovering => 4,
            Self::Unavailable => 5,
            Self::OwnershipLost => 6,
        }
    }
}

#[derive(Clone, Copy)]
struct Color(u8, u8, u8, u8);

struct Canvas {
    size: i32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(size: usize) -> Self {
        Self {
            size: size as i32,
            pixels: vec![0; size * size * 4],
        }
    }

    fn scale(&self, value: i32) -> i32 {
        (value * self.size + 15) / 32
    }

    fn pixel(&mut self, x: i32, y: i32, color: Color) {
        if !(0..self.size).contains(&x) || !(0..self.size).contains(&y) {
            return;
        }
        let index = ((self.size - 1 - y) * self.size + x) as usize * 4;
        self.pixels[index..index + 4].copy_from_slice(&[color.2, color.1, color.0, color.3]);
    }

    fn line(&mut self, from: (i32, i32), to: (i32, i32), width: i32, color: Color) {
        let (mut x0, mut y0) = from;
        let (x1, y1) = to;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            self.fill_square(x0, y0, width, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = error * 2;
            if twice >= dy {
                error += dy;
                x0 += sx;
            }
            if twice <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    fn fill_square(&mut self, x: i32, y: i32, width: i32, color: Color) {
        let radius = width / 2;
        for yy in y - radius..=y + radius {
            for xx in x - radius..=x + radius {
                self.pixel(xx, yy, color);
            }
        }
    }

    fn outline(&mut self, left: i32, top: i32, right: i32, bottom: i32, width: i32, color: Color) {
        self.line((left, top), (right, top), width, color);
        self.line((right, top), (right, bottom), width, color);
        self.line((right, bottom), (left, bottom), width, color);
        self.line((left, bottom), (left, top), width, color);
    }

    fn badge(&mut self, center: (i32, i32), color: Color) {
        let radius = (self.size / 8).max(2);
        for y in center.1 - radius..=center.1 + radius {
            for x in center.0 - radius..=center.0 + radius {
                let dx = x - center.0;
                let dy = y - center.1;
                if dx * dx + dy * dy <= radius * radius {
                    self.pixel(x, y, color);
                }
            }
        }
    }
}

fn create_icon(size: usize, state: TrayState) -> Result<HICON, String> {
    let mut canvas = Canvas::new(size);
    let stroke = (size as i32 / 16).max(1);

    // Dark under-stroke preserves contrast on light taskbars.
    let contrast_stroke = stroke + 2;
    canvas.outline(
        canvas.scale(6),
        canvas.scale(5),
        canvas.scale(22),
        canvas.scale(16),
        contrast_stroke,
        CONTRAST,
    );
    canvas.outline(
        canvas.scale(11),
        canvas.scale(10),
        canvas.scale(28),
        canvas.scale(21),
        contrast_stroke,
        CONTRAST,
    );
    let center = canvas.scale(19);
    canvas.line(
        (center, canvas.scale(21)),
        (center, canvas.scale(25)),
        contrast_stroke,
        CONTRAST,
    );
    canvas.line(
        (canvas.scale(15), canvas.scale(25)),
        (canvas.scale(23), canvas.scale(25)),
        contrast_stroke,
        CONTRAST,
    );

    // Rear display first; front display overlaps its lower-right corner.
    canvas.outline(
        canvas.scale(6),
        canvas.scale(5),
        canvas.scale(22),
        canvas.scale(16),
        stroke,
        WHITE,
    );
    canvas.outline(
        canvas.scale(11),
        canvas.scale(10),
        canvas.scale(28),
        canvas.scale(21),
        stroke,
        WHITE,
    );
    canvas.line(
        (center, canvas.scale(21)),
        (center, canvas.scale(25)),
        stroke,
        WHITE,
    );
    canvas.line(
        (canvas.scale(15), canvas.scale(25)),
        (canvas.scale(23), canvas.scale(25)),
        stroke,
        WHITE,
    );

    // External badge stays visible without competing with the display glyph.
    let accent = (canvas.scale(28), canvas.scale(23));
    let accent_width = (size as i32 / 16).max(1);
    match state {
        TrayState::NoTarget => {}
        TrayState::ConnectedLocal => canvas.badge(accent, GREEN),
        TrayState::Connecting => canvas.badge(accent, AMBER),
        TrayState::OwnershipPending => canvas.badge(accent, VIOLET),
        TrayState::Recovering => {
            canvas.badge(accent, AMBER);
            if size >= 24 {
                canvas.line(
                    (canvas.scale(27), canvas.scale(11)),
                    (canvas.scale(29), canvas.scale(9)),
                    accent_width,
                    AMBER,
                );
                canvas.line(
                    (canvas.scale(28), canvas.scale(15)),
                    (canvas.scale(30), canvas.scale(14)),
                    accent_width,
                    AMBER,
                );
            }
        }
        TrayState::Unavailable => {
            canvas.badge(accent, RED);
            if size >= 24 {
                canvas.line(
                    (canvas.scale(24), canvas.scale(12)),
                    (canvas.scale(27), canvas.scale(15)),
                    accent_width,
                    RED,
                );
            }
        }
        TrayState::OwnershipLost => canvas.badge(accent, RED),
    }

    let data = icon_resource(&canvas);
    let icon = unsafe {
        CreateIconFromResourceEx(
            data.as_ptr(),
            data.len() as u32,
            1,
            ICON_VERSION,
            size as i32,
            size as i32,
            0,
        )
    };
    if icon.is_null() {
        Err("could not create kvmctl tray icon".into())
    } else {
        Ok(icon)
    }
}

fn icon_resource(canvas: &Canvas) -> Vec<u8> {
    let size = canvas.size as usize;
    let mask_stride = size.div_ceil(32) * 4;
    let color_size = size * size * 4;
    let mask_size = mask_stride * size;
    let mut data = Vec::with_capacity(40 + color_size + mask_size);
    data.extend_from_slice(&40u32.to_le_bytes());
    data.extend_from_slice(&(size as i32).to_le_bytes());
    data.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&32u16.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&((color_size + mask_size) as u32).to_le_bytes());
    data.extend_from_slice(&0i32.to_le_bytes());
    data.extend_from_slice(&0i32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&canvas.pixels);
    data.resize(data.len() + mask_size, 0);
    data
}

fn destroy_icons(icons: &[HICON]) {
    for &icon in icons {
        if !icon.is_null() {
            unsafe { DestroyIcon(icon) };
        }
    }
}
