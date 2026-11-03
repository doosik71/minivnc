//! The connection bar of full-screen mode: minimize, restore and close
//! buttons centred at the top of the screen, drawn by the viewer since a
//! full-screen window has no title bar. It shows when the pointer touches
//! the top edge above it; margins on both sides of the buttons make it easy
//! to find on wide monitors.

/// Button size, as Windows caption buttons.
const BUTTON_W: u32 = 46;
/// Empty bar on each side of the buttons.
const MARGIN: u32 = 230;
pub const HEIGHT: u32 = 28;
/// How close to the top edge the pointer must come to show the bar.
const TRIGGER: f64 = 2.0;
const ICON: i32 = 10;

const BACKGROUND: u32 = 0x2b_2b_2b;
const HOVER: u32 = 0x50_50_50;
const CLOSE_HOVER: u32 = 0xc4_2b_1c;
const FOREGROUND: u32 = 0xff_ff_ff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Restore,
    Close,
}

const BUTTONS: [Button; 3] = [Button::Minimize, Button::Restore, Button::Close];

const BUTTONS_W: u32 = BUTTONS.len() as u32 * BUTTON_W;

/// Left edge of the buttons in a window `width` pixels wide.
fn left(width: u32) -> u32 {
    width.saturating_sub(BUTTONS_W) / 2
}

/// Horizontal extent of the bar, margins included.
fn span(width: u32) -> (u32, u32) {
    let start = left(width).saturating_sub(MARGIN);
    (start, (left(width) + BUTTONS_W + MARGIN).min(width))
}

fn in_span(width: u32, x: f64) -> bool {
    let (start, end) = span(width);
    x >= f64::from(start) && x < f64::from(end)
}

/// The pointer touches the top edge above the bar.
pub fn triggered(width: u32, (x, y): (f64, f64)) -> bool {
    y < TRIGGER && in_span(width, x)
}

/// The pointer is over the bar.
pub fn contains(width: u32, (x, y): (f64, f64)) -> bool {
    y < f64::from(HEIGHT) && in_span(width, x)
}

pub fn button_at(width: u32, pos: (f64, f64)) -> Option<Button> {
    let offset = pos.0 - f64::from(left(width));
    if !contains(width, pos) || offset < 0.0 {
        return None;
    }
    BUTTONS
        .get((offset / f64::from(BUTTON_W)) as usize)
        .copied()
}

/// Draws the bar over `buf`, a `width` x `height` image.
pub fn draw(buf: &mut [u32], width: u32, height: u32, hover: Option<Button>) {
    let mut put = |x: i32, y: i32, colour: u32| {
        if x >= 0 && y >= 0 && (x as u32) < width && (y as u32) < height {
            buf[y as usize * width as usize + x as usize] = colour;
        }
    };
    let (start, end) = span(width);
    for y in 0..HEIGHT as i32 {
        for x in start as i32..end as i32 {
            put(x, y, BACKGROUND);
        }
    }
    for (i, &button) in BUTTONS.iter().enumerate() {
        let x0 = (left(width) + i as u32 * BUTTON_W) as i32;
        let background = match hover {
            Some(h) if h == button && button == Button::Close => CLOSE_HOVER,
            Some(h) if h == button => HOVER,
            _ => BACKGROUND,
        };
        for y in 0..HEIGHT as i32 {
            for x in x0..x0 + BUTTON_W as i32 {
                put(x, y, background);
            }
        }
        // A 10x10 icon in the middle of the button.
        let (cx, cy) = (
            x0 + (BUTTON_W as i32 - ICON) / 2,
            (HEIGHT as i32 - ICON) / 2,
        );
        for d in 0..ICON {
            match button {
                Button::Minimize => put(cx + d, cy + ICON / 2, FOREGROUND),
                Button::Restore => {
                    // A window in front of another.
                    let (fx, fy, s) = (cx, cy + 2, ICON - 2);
                    if d < s {
                        put(fx + d, fy, FOREGROUND);
                        put(fx + d, fy + s - 1, FOREGROUND);
                        put(fx, fy + d, FOREGROUND);
                        put(fx + s - 1, fy + d, FOREGROUND);
                    }
                    if (2..ICON).contains(&d) {
                        put(cx + d, cy, FOREGROUND);
                    }
                    if d < s {
                        put(cx + ICON - 1, cy + d, FOREGROUND);
                    }
                }
                Button::Close => {
                    put(cx + d, cy + d, FOREGROUND);
                    put(cx + ICON - 1 - d, cy + d, FOREGROUND);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry() {
        // Buttons 138 pixels wide, centred in 2000: from 931 to 1069; with
        // the margins, the bar runs from 701 to 1299.
        assert!(triggered(2000, (701.0, 0.0)));
        assert!(triggered(2000, (1298.0, 1.0)));
        assert!(!triggered(2000, (700.0, 0.0)));
        assert!(!triggered(2000, (1299.0, 0.0)));
        assert!(!triggered(2000, (1000.0, 5.0)));
        assert!(contains(2000, (750.0, 20.0)));
        assert_eq!(button_at(2000, (750.0, 10.0)), None);
        assert_eq!(button_at(2000, (940.0, 10.0)), Some(Button::Minimize));
        assert_eq!(button_at(2000, (1000.0, 10.0)), Some(Button::Restore));
        assert_eq!(button_at(2000, (1068.0, 10.0)), Some(Button::Close));
        assert_eq!(button_at(2000, (1100.0, 10.0)), None);
        assert_eq!(button_at(2000, (1000.0, 30.0)), None);
        // Narrower than the bar: it spans the window.
        assert!(triggered(300, (0.0, 0.0)) && triggered(300, (299.0, 0.0)));
    }

    #[test]
    fn draws_inside_the_image() {
        let (w, h) = (100, 20);
        let mut buf = vec![0; (w * h) as usize];
        draw(&mut buf, w, h, Some(Button::Close));
        assert!(buf.contains(&CLOSE_HOVER) && buf.contains(&FOREGROUND));
    }
}
