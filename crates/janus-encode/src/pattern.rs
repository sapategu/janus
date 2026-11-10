//! Moving colour-bar test image (tight BGRA). A bright bar sweeps across
//! once per second so motion and frame pacing are visible on the receiver.

pub fn draw(frame: u64, frame_rate: u32, width: u32, height: u32, out: &mut Vec<u8>) {
    const BARS: [[u8; 3]; 7] = [
        [235, 235, 235],
        [16, 235, 235],
        [235, 235, 16],
        [16, 235, 16],
        [235, 16, 235],
        [16, 16, 235],
        [235, 16, 16],
    ];
    let (w, h) = (width as usize, height as usize);
    out.clear();
    out.resize(w * h * 4, 0xff);
    let rate = u64::from(frame_rate.max(1));
    let sweep = ((frame % rate) as usize * w) / rate as usize;
    let sweep_width = (w / 40).max(2);
    for y in 0..h {
        for x in 0..w {
            let pixel = &mut out[(y * w + x) * 4..(y * w + x) * 4 + 4];
            let colour = if x.abs_diff(sweep) < sweep_width {
                [255, 255, 255]
            } else if y > h * 3 / 4 {
                // Lower quarter: grey ramp that scrolls with time.
                let level = ((x + frame as usize * 4) % w * 255 / w.max(1)) as u8;
                [level, level, level]
            } else {
                BARS[x * BARS.len() / w.max(1)]
            };
            pixel[..3].copy_from_slice(&colour);
        }
    }
}
