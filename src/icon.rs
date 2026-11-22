// Kennel's app icon, drawn in code: a white doghouse with an arched door on an
// indigo rounded tile. Shared by the app (window/taskbar icon) and build.rs
// (the .ico embedded in kennel.exe), so this file must stay dependency-free.

/// RGBA pixels (row-major, top-down) of the icon at `size` x `size`.
#[allow(dead_code)]
pub fn icon_rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4; // supersampling per axis, for smooth edges
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if let Some([cr, cg, cb]) = icon_color(x, y) {
                        r += cr as f32;
                        g += cg as f32;
                        b += cb as f32;
                        a += 1.0;
                    }
                }
            }
            let n = (SS * SS) as f32;
            if a > 0.0 {
                out.extend([(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n * 255.0).round() as u8]);
            } else {
                out.extend([0, 0, 0, 0]);
            }
        }
    }
    out
}

/// Color at a point in unit coordinates (0..1), or None if transparent.
fn icon_color(x: f32, y: f32) -> Option<[u8; 3]> {
    // Rounded tile with a gentle top-to-bottom gradient.
    if !in_rounded_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.23) {
        return None;
    }
    let t = y.clamp(0.0, 1.0);
    let tile = [lerp(0x8C, 0x5E, t), lerp(0x9A, 0x6C, t), lerp(0xFF, 0xF2, t)];
    // Arched door (dark), on top of the house.
    let (dx0, dx1, dtop, dbot, dr) = (0.415, 0.585, 0.60, 0.79, 0.085);
    let in_door = (x >= dx0 && x <= dx1 && y >= dtop && y <= dbot)
        || ((x - 0.5).powi(2) + (y - dtop).powi(2) <= dr * dr && y <= dtop);
    if in_door {
        return Some([0x2B, 0x30, 0x70]);
    }
    // House: body plus a roof that overhangs it a little.
    let in_body = (0.25..=0.75).contains(&x) && (0.46..=0.79).contains(&y);
    let in_roof = in_triangle(x, y, (0.5, 0.17), (0.15, 0.50), (0.85, 0.50));
    if in_body || in_roof {
        return Some([0xF6, 0xF7, 0xFF]);
    }
    Some(tile)
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn in_rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn in_triangle(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1);
    let p = (x, y);
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// A Windows .ico holding the icon at each of `sizes` (32-bit BMP entries).
#[allow(dead_code)]
pub fn icon_ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &s in sizes {
        let rgba = icon_rgba(s);
        let mask_row = (s.div_ceil(32) * 4) as usize; // 1bpp AND mask, 4-byte aligned rows
        let mut img = Vec::new();
        // BITMAPINFOHEADER (height doubled: XOR image + AND mask)
        for v in [40u32, s, s * 2] {
            img.extend(v.to_le_bytes());
        }
        img.extend(1u16.to_le_bytes()); // planes
        img.extend(32u16.to_le_bytes()); // bpp
        for _ in 0..6 {
            img.extend(0u32.to_le_bytes()); // compression, sizes, resolution, colors
        }
        // Pixels: BGRA, bottom-up.
        for y in (0..s).rev() {
            for x in 0..s {
                let i = ((y * s + x) * 4) as usize;
                img.extend([rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        img.extend(std::iter::repeat_n(0u8, mask_row * s as usize));
        images.push(img);
    }
    let mut ico = Vec::new();
    ico.extend(0u16.to_le_bytes()); // reserved
    ico.extend(1u16.to_le_bytes()); // type: icon
    ico.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        ico.extend([dim, dim, 0, 0]);
        ico.extend(1u16.to_le_bytes()); // planes
        ico.extend(32u16.to_le_bytes()); // bpp
        ico.extend((img.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        ico.extend(img);
    }
    ico
}
