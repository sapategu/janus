//! Packed 32-bit frame conversion: any channel order and row stride in, tight
//! BGRA at an even target size out. Downscaling is area averaging (each output
//! pixel is the coverage-weighted mean of the source pixels under it), which
//! keeps thin text strokes intact; nearest-neighbour sampling made 2560→1920
//! text visibly jagged on the TV. Equal sizes are a plain channel copy.

/// Byte order of a packed 32-bit pixel (PipeWire `BGRx`, `RGBx`, `BGRA`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Bytes B, G, R, X/A.
    Bgrx,
    /// Bytes R, G, B, X/A.
    Rgbx,
    /// Bytes X/A, R, G, B.
    Xrgb,
    /// Bytes X/A, B, G, R.
    Xbgr,
}

impl PixelFormat {
    /// Offsets of (B, G, R) within a pixel.
    fn bgr(self) -> (usize, usize, usize) {
        match self {
            Self::Bgrx => (0, 1, 2),
            Self::Rgbx => (2, 1, 0),
            Self::Xrgb => (3, 2, 1),
            Self::Xbgr => (1, 2, 3),
        }
    }
}

/// Largest even size ≤ (`max_width`, `max_height`) with the source aspect.
pub fn fit(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let scale = f64::min(
        1.0,
        f64::min(
            f64::from(max_width) / f64::from(width),
            f64::from(max_height) / f64::from(height),
        ),
    );
    let even = |value: f64| ((value as u32) & !1).max(2);
    (
        even(f64::from(width) * scale),
        even(f64::from(height) * scale),
    )
}

/// Converts `source` (`width`×`height`, `stride` bytes per row) into `out`
/// as tight BGRA at `target`. Returns `false` if the input is too short.
pub fn to_bgra(
    source: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    target: (u32, u32),
    out: &mut Vec<u8>,
) -> bool {
    let (width, height) = (width as usize, height as usize);
    let (tw, th) = (target.0 as usize, target.1 as usize);
    if width == 0 || height == 0 || tw == 0 || th == 0 || stride < width * 4 {
        return false;
    }
    if source.len() < stride * (height - 1) + width * 4 {
        return false;
    }
    let (b, g, r) = format.bgr();
    out.clear();
    out.resize(tw * th * 4, 0xff);
    if (tw, th) == (width, height) {
        for y in 0..th {
            let row = &source[y * stride..y * stride + width * 4];
            let dst = &mut out[y * tw * 4..(y + 1) * tw * 4];
            for (d, pixel) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                d[0] = pixel[b];
                d[1] = pixel[g];
                d[2] = pixel[r];
            }
        }
        return true;
    }
    let columns = Taps::new(width, tw);
    let rows = Taps::new(height, th);
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 4)
        .min(th);
    let band = th.div_ceil(threads);
    std::thread::scope(|scope| {
        for (index, chunk) in out.chunks_mut(band * tw * 4).enumerate() {
            let (columns, rows) = (&columns, &rows);
            scope.spawn(move || {
                let first = index * band;
                downscale_band(source, stride, (b, g, r), columns, rows, first, tw, chunk);
            });
        }
    });
    true
}

/// Coverage taps in flat form: output `i` reads `count` source indices from
/// `first[i]` with weights `weights[i * count..]` (16-bit fixed point).
struct Taps {
    count: usize,
    first: Vec<usize>,
    weights: Vec<u32>,
}
impl Taps {
    fn new(source: usize, target: usize) -> Self {
        let lists = taps(source, target);
        let count = lists.iter().map(Vec::len).max().unwrap_or(1);
        let mut first = Vec::with_capacity(target);
        let mut weights = vec![0; target * count];
        for (i, list) in lists.iter().enumerate() {
            // Shift the window left at the right edge so it stays in range.
            let start = list[0].0.min(source.saturating_sub(count));
            first.push(start);
            for &(s, w) in list {
                weights[i * count + (s - start)] = w;
            }
        }
        Self {
            count,
            first,
            weights,
        }
    }
}

/// Output rows `first..` of one band: each needed source row is filtered
/// horizontally once into a small cache, then rows are blended vertically.
#[allow(clippy::too_many_arguments)]
fn downscale_band(
    source: &[u8],
    stride: usize,
    (b, g, r): (usize, usize, usize),
    columns: &Taps,
    rows: &Taps,
    first: usize,
    tw: usize,
    out: &mut [u8],
) {
    let k = rows.count;
    let mut cache: Vec<(usize, Vec<u32>)> = (0..k).map(|_| (usize::MAX, vec![0; tw * 3])).collect();
    let filter = |sy: usize, dst: &mut [u32]| {
        let row = &source[sy * stride..];
        for x in 0..tw {
            let base = columns.first[x];
            let weights = &columns.weights[x * columns.count..(x + 1) * columns.count];
            let (mut bb, mut gg, mut rr) = (0u32, 0u32, 0u32);
            for (j, &w) in weights.iter().enumerate() {
                let p = &row[(base + j) * 4..(base + j) * 4 + 4];
                bb += w * u32::from(p[b]);
                gg += w * u32::from(p[g]);
                rr += w * u32::from(p[r]);
            }
            dst[x * 3] = bb;
            dst[x * 3 + 1] = gg;
            dst[x * 3 + 2] = rr;
        }
    };
    for (offset, dst) in out.chunks_exact_mut(tw * 4).enumerate() {
        let y = first + offset;
        let base = rows.first[y];
        for j in 0..k {
            let sy = base + j;
            let slot = sy % k;
            if cache[slot].0 != sy {
                filter(sy, &mut cache[slot].1);
                cache[slot].0 = sy;
            }
        }
        let weights = &rows.weights[y * k..(y + 1) * k];
        // Resolve the cached rows once; the inner loop is plain multiply-adds.
        let lanes: Vec<(u64, &[u32])> = weights
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 0)
            .map(|(j, &w)| (u64::from(w), cache[(base + j) % k].1.as_slice()))
            .collect();
        for (x, pixel) in dst.chunks_exact_mut(4).enumerate() {
            for (c, value) in pixel[..3].iter_mut().enumerate() {
                let i = x * 3 + c;
                let sum: u64 = lanes.iter().map(|(w, lane)| w * u64::from(lane[i])).sum();
                // 16-bit weights on both axes: 2^32 is one pixel.
                *value = ((sum + (1 << 31)) >> 32).min(255) as u8;
            }
        }
    }
}

/// For each output index, the source indices it covers and their coverage
/// weights in 16-bit fixed point, summing to exactly 65536. Upscaling (never
/// planned) degrades to the covering source pixel.
fn taps(source: usize, target: usize) -> Vec<Vec<(usize, u32)>> {
    let ratio = source as f64 / target as f64;
    (0..target)
        .map(|i| {
            let start = i as f64 * ratio;
            let end = ((i + 1) as f64 * ratio).min(source as f64);
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).clamp(first + 1, source);
            let span = (end - start).max(f64::EPSILON);
            let mut list: Vec<(usize, u32)> = (first..last)
                .map(|s| {
                    let covered = (end.min((s + 1) as f64) - start.max(s as f64)).max(0.0);
                    (s, (covered / span * 65536.0).round() as u32)
                })
                .filter(|(_, w)| *w > 0)
                .collect();
            if list.is_empty() {
                list.push((first.min(source - 1), 0));
            }
            let total: u32 = list.iter().map(|(_, w)| *w).sum();
            let last = list.len() - 1;
            list[last].1 = (i64::from(list[last].1) + 65536 - i64::from(total)).max(0) as u32;
            list
        })
        .collect()
}
