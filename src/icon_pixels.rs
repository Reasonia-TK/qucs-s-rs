// Shared by the GUI (window icon) and build.rs (the .exe icon): a resistor
// zig-zag on a rounded navy tile. Kept free of crate dependencies on purpose.

/// RGBA pixels (straight alpha) of the application icon at `n` x `n`.
pub fn icon_rgba(n: usize) -> Vec<u8> {
    let k = n as f32 / 64.0;
    let mut px = vec![0u8; n * n * 4];
    let bg = [0x1f, 0x2a, 0x44, 0xff];
    let r = 12.0 * k;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, n as f32 - r);
            let cy = fy.clamp(r, n as f32 - r);
            if (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r {
                px[(y * n + x) * 4..(y * n + x) * 4 + 4].copy_from_slice(&bg);
            }
        }
    }
    let pts = [(4.0, 34.0), (14.0, 34.0), (18.0, 22.0), (24.0, 46.0), (30.0, 22.0), (36.0, 46.0), (42.0, 22.0), (46.0, 34.0), (60.0, 34.0)];
    let gold = [0xf2, 0xc1, 0x4e, 0xff];
    let rad = (2.3 * k).max(1.0);
    for w in pts.windows(2) {
        let ((x0, y0), (x1, y1)): ((f32, f32), (f32, f32)) = (w[0], w[1]);
        let (x0, y0, x1, y1) = (x0 * k, y0 * k, x1 * k, y1 * k);
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()) * 3.0) as usize + 1;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
            let ri = rad.ceil() as i32;
            for dy in -ri..=ri {
                for dx in -ri..=ri {
                    if ((dx * dx + dy * dy) as f32) <= rad * rad {
                        let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                        if xx >= 0 && yy >= 0 && (xx as usize) < n && (yy as usize) < n {
                            let o = (yy as usize * n + xx as usize) * 4;
                            px[o..o + 4].copy_from_slice(&gold);
                        }
                    }
                }
            }
        }
    }
    px
}
