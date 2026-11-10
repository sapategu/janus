//! Offline QA assets and instrument observations, never a capture backend.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

#[doc(hidden)]
pub const WIDTH: usize = 320;
#[doc(hidden)]
pub const HEIGHT: usize = 180;
#[doc(hidden)]
pub const FPS: u32 = 60;
#[doc(hidden)]
pub const RATE: u32 = 48_000;
#[doc(hidden)]
pub const SAMPLES_PER_FRAME: u32 = RATE / FPS;
#[doc(hidden)]
pub const MAX_OBSERVATIONS: usize = 10_000;

#[doc(hidden)]
pub fn flash(frame: u32) -> bool {
    frame != 0 && frame.is_multiple_of(FPS)
}

#[doc(hidden)]
pub fn picture(frame: u32, pixels: &mut [u8]) {
    pixels.fill(128); // Neutral chroma, limited-range grayscale.
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            pixels[y * WIDTH + x] = if y < 120 {
                if flash(frame) {
                    235
                } else if x.abs_diff((frame as usize * 5) % WIDTH) < 8 {
                    180
                } else {
                    16 + (((x / 20 + y / 20) % 2) * 24) as u8
                }
            } else if (140..160).contains(&y) && frame & (1 << (x / 10)) != 0 {
                235 // 32-bit frame ID, least-significant bit on the left.
            } else {
                16
            };
        }
    }
}

#[doc(hidden)]
pub fn audio_sample(sample: u32) -> i16 {
    let frame = sample / SAMPLES_PER_FRAME;
    match sample % SAMPLES_PER_FRAME {
        0..24 if flash(frame) => 12_000,
        24..48 if flash(frame) => -12_000,
        _ => 0,
    }
}

#[doc(hidden)]
pub fn video(out: &mut impl Write, frames: u32) -> Result<()> {
    writeln!(
        out,
        "YUV4MPEG2 W{WIDTH} H{HEIGHT} F{FPS}:1 Ip A1:1 C420jpeg"
    )?;
    let mut pixels = vec![128; WIDTH * HEIGHT * 3 / 2];
    for frame in 0..frames {
        picture(frame, &mut pixels);
        out.write_all(b"FRAME\n")?;
        out.write_all(&pixels)?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn audio(out: &mut impl Write, frames: u32) -> Result<()> {
    let samples = frames * SAMPLES_PER_FRAME;
    let data_bytes = samples * 4; // Two channels of signed 16-bit PCM.
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_bytes).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16_u32.to_le_bytes())?;
    out.write_all(&1_u16.to_le_bytes())?;
    out.write_all(&2_u16.to_le_bytes())?;
    out.write_all(&RATE.to_le_bytes())?;
    out.write_all(&(RATE * 4).to_le_bytes())?;
    out.write_all(&4_u16.to_le_bytes())?;
    out.write_all(&16_u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_bytes.to_le_bytes())?;
    for sample in 0..samples {
        let pcm = audio_sample(sample).to_le_bytes();
        out.write_all(&pcm)?;
        out.write_all(&pcm)?;
    }
    Ok(())
}

pub fn generate(output: &Path, frames: u32) -> Result<()> {
    ensure!((120..=3600).contains(&frames), "frames must be 120..=3600");
    // Refuse existing directories; never overwrite a baseline recording.
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output).context("fixture directory must be new")?;
    let mut y4m = BufWriter::new(File::create_new(output.join("pattern.y4m"))?);
    video(&mut y4m, frames)?;
    y4m.flush()?;
    let mut wav = BufWriter::new(File::create_new(output.join("impulses.wav"))?);
    audio(&mut wav, frames)?;
    wav.flush()?;
    let events: Vec<_> = (FPS..frames)
        .step_by(FPS as usize)
        .enumerate()
        .map(|(id, frame)| {
            serde_json::json!({
                "event_id": id, "frame": frame,
                "pts_us": u64::from(frame) * 1_000_000 / u64::from(FPS),
                "audio_sample": frame * SAMPLES_PER_FRAME
            })
        })
        .collect();
    // Written last: a directory without a manifest is incomplete.
    let manifest = serde_json::json!({
        "schema": 1, "synthetic_only": true, "physically_calibrated": false,
        "width": WIDTH, "height": HEIGHT, "fps": FPS, "frames": frames,
        "sample_rate": RATE, "channels": 2,
        "video": "pattern.y4m", "audio": "impulses.wav", "events": events,
        "note": "Shared intended PTS does not establish actual display/audio timing. Start playback muted."
    });
    let mut out = File::create_new(output.join("manifest.json"))?;
    serde_json::to_writer_pretty(&mut out, &manifest)?;
    out.write_all(b"\n")?;
    println!(
        "Fixture: {} (synthetic; calibration pending)",
        output.display()
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[doc(hidden)]
pub struct Observation {
    pub event_id: usize,
    pub capture_us: u64,
    pub source_audio_us: Option<u64>,
    pub display_us: Option<u64>,
    pub audio_us: Option<u64>,
}

#[derive(Debug, Serialize)]
#[doc(hidden)]
pub struct Summary {
    pub physical_provenance_verified: bool,
    pub declared_calibration_error_us: u32,
    pub expected_events: usize,
    pub video_misses: usize,
    pub audio_misses: usize,
    pub audio_reference_misses: usize,
    pub audio_latency_events: usize,
    pub paired_events: usize,
    pub video_latency_p95_us: Option<i64>,
    pub audio_latency_p95_us: Option<i64>,
    pub absolute_skew_p95_us: Option<i64>,
    pub signed_skew_min_us: Option<i64>,
    pub signed_skew_max_us: Option<i64>,
}

#[doc(hidden)]
pub fn percentile(values: &mut [i64]) -> Option<i64> {
    values.sort_unstable();
    values
        .get((values.len() * 95).div_ceil(100).saturating_sub(1))
        .copied()
}

#[doc(hidden)]
pub fn summarize(rows: &[Observation], expected: usize, calibration_us: u32) -> Result<Summary> {
    ensure!(
        expected > 0 && expected <= MAX_OBSERVATIONS,
        "invalid event count"
    );
    ensure!(
        rows.len() == expected,
        "include every event, using null for misses"
    );
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut skew = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        ensure!(
            row.event_id == index,
            "event IDs must be consecutive from zero"
        );
        ensure!(
            index == 0 || row.capture_us > rows[index - 1].capture_us,
            "capture times must increase"
        );
        ensure!(
            [
                Some(row.capture_us),
                row.source_audio_us,
                row.display_us,
                row.audio_us
            ]
            .into_iter()
            .flatten()
            .all(|time| time <= i64::MAX as u64),
            "timestamp out of range"
        );
        for (reference, time, values) in [
            (Some(row.capture_us), row.display_us, &mut video),
            (row.source_audio_us, row.audio_us, &mut audio),
        ] {
            if let (Some(reference), Some(time)) = (reference, time) {
                ensure!(
                    time >= reference,
                    "output precedes capture; use one instrument clock"
                );
                values.push((time - reference) as i64);
            }
        }
        if let (Some(display), Some(audio)) = (row.display_us, row.audio_us) {
            skew.push(audio as i64 - display as i64);
        }
    }
    let mut absolute_skew: Vec<_> = skew.iter().map(|skew| skew.abs()).collect();
    Ok(Summary {
        physical_provenance_verified: false,
        declared_calibration_error_us: calibration_us,
        expected_events: expected,
        video_misses: expected - video.len(),
        audio_misses: rows.iter().filter(|row| row.audio_us.is_none()).count(),
        audio_reference_misses: rows
            .iter()
            .filter(|row| row.source_audio_us.is_none())
            .count(),
        audio_latency_events: audio.len(),
        paired_events: skew.len(),
        video_latency_p95_us: percentile(&mut video),
        audio_latency_p95_us: percentile(&mut audio),
        absolute_skew_p95_us: percentile(&mut absolute_skew),
        signed_skew_min_us: skew.iter().min().copied(),
        signed_skew_max_us: skew.iter().max().copied(),
    })
}

pub fn correlate(path: &Path, expected: usize, calibration_us: u32) -> Result<()> {
    use std::io::Read;
    let mut bytes = Vec::new();
    File::open(path)?.take(2_097_153).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2_097_152, "observations exceed 2 MiB");
    let rows: Vec<Observation> = serde_json::from_slice(&bytes)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summarize(&rows, expected, calibration_us)?)?
    );
    Ok(())
}
