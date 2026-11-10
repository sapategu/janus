# A05 measurement procedure

All generated files stay under `target/`. No fixture command starts playback, captures private content, opens host sockets or changes a network interface.

```sh
cargo run --locked --bin janus -- diagnostics fixture target/fixtures/flash-impulse
cargo run --locked --bin janus -- diagnostics correlate docs/fixtures/observations.synthetic.json --expected-events 2 --calibration-error-us 1
cargo run --locked --bin janus -- diagnostics simulate --frames 3600
cargo test --locked -p janus-session impaired_packets -- --nocapture
```

Fixture output refuses an existing directory. Choose a fresh path for each run. The default is 180 frames of 320×180 60 Hz limited-range YUV420 video and 48 kHz stereo 16-bit PCM. A moving bar/grid exercises motion; the bottom band encodes the 32-bit frame ID, least-significant bit on the left. Frames 60, 120, … flash the upper two-thirds white. A 1 ms bipolar pulse begins at exactly the same intended PTS in the audio file. `manifest.json` records frame/sample/event IDs; it is written last, so an incomplete generation has no completed manifest. Start playback muted and use safe levels. Neither intended PTS nor a software playback timestamp establishes physical flash/impulse synchronization.

For manual inspection, FFmpeg can mux the two files without adding an encoder:

```sh
ffmpeg -i target/fixtures/flash-impulse/pattern.y4m -i target/fixtures/flash-impulse/impulses.wav -c:v rawvideo -c:a pcm_s16le target/fixtures/flash-impulse/pattern.nut
```

## Physical calibration gate

1. Record fixture revision, executable/lockfile hashes, OS/GPU/driver/provider, source/receiver display refresh and processing modes, audio route and network profile using the [asset inventory](../foundation.md). Retain the original instrument recording; never commit private screen/audio recordings.
2. Use one common instrument clock for source light, receiver light and analog audio. Measure fixture/channel delay on a common local reference before casting, including camera cadence/rolling shutter, photodiode threshold, audio channel lag and instrument resolution. Repeat before/after runs and retain signed offsets and worst-case calibration error. Do not infer these offsets from Janus's clock estimator or subtract assumed display delays.
3. For every planned flash event record `event_id` (consecutive from zero), `capture_us` (source's physically observed light transition), `source_audio_us` (sender analog reference impulse), `display_us`, `audio_us`. Use a JSON array and one clock domain. Put `null` for a missed receiver transition/output or missing sender audio reference, retaining its source event. Declare the expected count separately; omitted rows, duplicates, reversed clocks and oversized files are rejected. The supplied sample is **synthetic** and includes a miss.
4. Run `diagnostics correlate <observations.json> --expected-events <N> --calibration-error-us <measured worst-case error>`. It reports nearest-rank p95 latency, absolute skew, signed audio-minus-video skew and miss counts. Audio latency uses its separately observed sender analog reference; missing references cannot be replaced by the source light timestamp. Lip sync is receiver audio time minus receiver light time. Percentiles cover successful observations only; always publish denominators. A supplied calibration number does not verify physical provenance or confer a passing product qualification. Keep the calibration recording with results.
5. Follow PRD §3: at least 1,000 transitions over three E/W runs per qualified path, plus separate 30-minute continuity runs, startup/recovery and Stop. Generated 3-second assets are a fixture sanity check, not that baseline.

## Packet and clock baselines

`impaired_packets` uses the existing test-only Quinn socket seam. It shapes actual QUIC datagrams on a shared 8 Mb/s simulated link with 2–5 ms one-way delay/jitter and every eleventh datagram lost, before TLS/control delivery. The bounded inbox holds at most 256 datagrams per endpoint and 65,535 bytes per datagram; excess is dropped before allocation. Test counters include loss, overflow and peak queued packet payload bytes. It verifies authenticated Janus-native control recovery and Stop; it is not receiver-media/Wi-Fi throughput or process-memory evidence. The loss schedule is deterministic; real runtime scheduling may change packet counts. No kernel network-shaper settings are changed.

`simulate` retains queue peak/resource counters and the affine clock driver. Clock tests cover ±200 ppm, path asymmetry, stale samples and bounded history. Queue/packet bytes exclude allocator, GPU, QUIC windows and process overhead.

Physical calibration, baseline recording and native total-memory measurements are still unavailable. A05 remains open until those measurements are retained.


Retained software evidence: [baseline.synthetic.json](baseline.synthetic.json) contains the generated asset byte counts/SHA-256 hashes, simulation and sample correlation. One observed impaired-control run sent 85 datagrams, deliberately lost 7, overflowed none and peaked at 2,954 queued payload bytes. These packet counts depend on runtime scheduling and are not golden values or physical network measurements.
