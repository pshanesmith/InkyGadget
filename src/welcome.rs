//! The screen shown while the library is empty: how to add photos.

use crate::overlay::{DISPLAY_H, DISPLAY_W};
use crate::palette::{BLACK, BLUE, WHITE};
use font8x8::legacy::BASIC_LEGACY;

fn text(buf: &mut [u8], x0: usize, y0: usize, s: &str, scale: usize, colour: u8) {
    for (n, ch) in s.chars().enumerate() {
        let g = BASIC_LEGACY[(ch as usize).min(127)];
        for (row, bits) in g.iter().enumerate() {
            for col in 0..8 {
                if bits >> col & 1 == 1 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (x, y) = (x0 + (n * 8 + col) * scale + dx, y0 + row * scale + dy);
                            if x < DISPLAY_W && y < DISPLAY_H {
                                buf[y * DISPLAY_W + x] = colour;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn centred(buf: &mut [u8], y: usize, s: &str, scale: usize, colour: u8) {
    let w = s.chars().count() * 8 * scale;
    text(buf, DISPLAY_W.saturating_sub(w) / 2, y, s, scale, colour);
}

/// `host` is the Pi's name (reachable as `host.local`), `password` the share password if known.
pub fn render(host: &str, user: &str, password: Option<&str>) -> Vec<u8> {
    let mut b = vec![WHITE; DISPLAY_W * DISPLAY_H];
    for x in 0..DISPLAY_W {
        for y in 0..10 {
            b[y * DISPLAY_W + x] = BLUE;
            b[(DISPLAY_H - 1 - y) * DISPLAY_W + x] = BLUE;
        }
    }
    centred(&mut b, 50, "InkyGadget", 6, BLACK);
    centred(&mut b, 130, "Add photos to start", 3, BLACK);
    centred(&mut b, 200, &format!("smb://{host}.local/Images"), 3, BLACK);
    centred(&mut b, 250, &format!("(Windows: \\\\{host}.local\\Images)"), 2, BLACK);
    centred(&mut b, 300, &format!("User: {user}"), 3, BLACK);
    centred(&mut b, 340, &format!("Password: {}", password.unwrap_or("see /etc/inkyframe/share-password")), 3, BLACK);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_text_inside_the_frame() {
        let b = render("inky", "inkyframe", Some("pw1234"));
        assert_eq!(b.len(), DISPLAY_W * DISPLAY_H);
        assert!(b.iter().filter(|&&p| p == BLACK).count() > 2000);
        assert_eq!(b[5 * DISPLAY_W + 5], BLUE);
    }

    #[test]
    fn long_text_never_panics() {
        render(&"x".repeat(200), &"y".repeat(200), Some(&"z".repeat(200)));
    }
}
