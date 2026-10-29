//! Turns native samples into Anya's chunks: downmix to mono, resample to
//! [`SAMPLE_RATE`], apply gain, quantise to s16le and cut into
//! [`CHUNK_BYTES`] pieces.

use std::f64::consts::PI;

use anya_core::audio::{CHUNK_BYTES, Chunk, SAMPLE_RATE};

/// Streaming conversion from one device format to [`Chunk`]s.
///
/// Allocates only while its scratch buffers grow and once per emitted chunk,
/// so it is safe to drive from an audio callback.
pub(crate) struct Converter {
    channels: usize,
    gain: i32,
    resampler: Resampler,
    mono: Vec<f32>,
    out: Vec<f32>,
    pending: Vec<u8>,
}

impl Converter {
    /// A converter for interleaved audio at `rate` Hz with `channels`
    /// channels. `gain` below 1 counts as 1.
    pub(crate) fn new(rate: u32, channels: u16, gain: i32) -> Converter {
        Converter {
            channels: usize::from(channels.max(1)),
            gain: gain.max(1),
            resampler: Resampler::new(rate),
            mono: Vec::new(),
            out: Vec::new(),
            pending: Vec::with_capacity(CHUNK_BYTES),
        }
    }

    /// Converts whole frames of `interleaved` and calls `emit` for every
    /// chunk completed. A trailing partial frame is ignored.
    pub(crate) fn push<S: Copy>(
        &mut self,
        interleaved: &[S],
        to_f32: impl Fn(S) -> f32,
        mut emit: impl FnMut(Chunk),
    ) {
        let scale = 1.0 / self.channels as f32;
        self.mono.clear();
        self.mono.extend(
            interleaved
                .chunks_exact(self.channels)
                .map(|frame| frame.iter().map(|&s| to_f32(s)).sum::<f32>() * scale),
        );
        self.out.clear();
        self.resampler.process(&self.mono, &mut self.out);
        self.emit_out(&mut emit);
    }

    /// Flushes the resampler and emits the last chunk, padded with silence.
    pub(crate) fn finish(&mut self, mut emit: impl FnMut(Chunk)) {
        self.out.clear();
        self.resampler.finish(&mut self.out);
        self.emit_out(&mut emit);
        if !self.pending.is_empty() {
            self.pending.resize(CHUNK_BYTES, 0);
            emit(Chunk(std::mem::take(&mut self.pending)));
        }
    }

    fn emit_out(&mut self, emit: &mut impl FnMut(Chunk)) {
        for &x in &self.out {
            let sample = amplify(quantize(x), self.gain);
            self.pending.extend_from_slice(&sample.to_le_bytes());
            if self.pending.len() == CHUNK_BYTES {
                let full = std::mem::replace(&mut self.pending, Vec::with_capacity(CHUNK_BYTES));
                emit(Chunk(full));
            }
        }
    }
}

/// Full scale is ±1.0; an i16 sample divided by 32768 comes back unchanged.
fn quantize(x: f32) -> i16 {
    (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

/// Multiplies by an integer gain, saturating at the i16 range.
fn amplify(sample: i16, gain: i32) -> i16 {
    if gain <= 1 {
        return sample;
    }
    i32::from(sample)
        .saturating_mul(gain)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Kernel table entries per input sample.
const RESOLUTION: usize = 256;
/// Zero crossings of the sinc on each side of the centre.
const ZERO_CROSSINGS: f64 = 16.0;
/// Cutoff as a fraction of the lower Nyquist frequency.
const PASSBAND: f64 = 0.92;

enum Resampler {
    Passthrough,
    Sinc(Sinc),
}

impl Resampler {
    fn new(rate: u32) -> Resampler {
        if rate == SAMPLE_RATE || rate == 0 {
            Resampler::Passthrough
        } else {
            Resampler::Sinc(Sinc::new(rate))
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        match self {
            Resampler::Passthrough => out.extend_from_slice(input),
            Resampler::Sinc(sinc) => {
                sinc.history.extend_from_slice(input);
                sinc.produce(out);
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<f32>) {
        if let Resampler::Sinc(sinc) = self {
            sinc.history.extend(std::iter::repeat_n(0.0, sinc.half));
            sinc.produce(out);
        }
    }
}

/// Band-limited resampling with a Blackman-windowed sinc.
///
/// The input is padded with `half` zeros in front, so a padded index is the
/// real index plus `half`. Output `n` sits at real input position
/// `n * in_rate / out_rate`, kept as an exact fraction so long captures do
/// not drift. Feeding the whole input yields `ceil(len * out / in)` samples.
struct Sinc {
    in_rate: u64,
    out_rate: u64,
    half: usize,
    table: Vec<f32>,
    /// Padded samples from padded index `base` on.
    history: Vec<f32>,
    base: u64,
    next: u64,
}

impl Sinc {
    fn new(rate: u32) -> Sinc {
        let in_rate = u64::from(rate);
        let out_rate = u64::from(SAMPLE_RATE);
        // Cutoff in cycles per input sample.
        let fc = 0.5 * (out_rate as f64 / in_rate as f64).min(1.0) * PASSBAND;
        let half = (ZERO_CROSSINGS / (2.0 * fc)).ceil() as usize;
        let table = (0..half * RESOLUTION + 2)
            .map(|i| {
                let d = i as f64 / RESOLUTION as f64;
                if d >= half as f64 {
                    return 0.0;
                }
                let x = 2.0 * fc * d;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                let w = d / half as f64;
                let window = 0.42 + 0.5 * (PI * w).cos() + 0.08 * (2.0 * PI * w).cos();
                (2.0 * fc * sinc * window) as f32
            })
            .collect();
        Sinc {
            in_rate,
            out_rate,
            half,
            table,
            history: vec![0.0; half],
            base: 0,
            next: 0,
        }
    }

    /// The kernel at distance `d` input samples, `|d| <= half`.
    fn kernel(&self, d: f32) -> f32 {
        let pos = d.abs() * RESOLUTION as f32;
        let i = pos as usize;
        let t = pos - i as f32;
        self.table[i] * (1.0 - t) + self.table[i + 1] * t
    }

    fn produce(&mut self, out: &mut Vec<f32>) {
        let taps = 2 * self.half;
        let end = self.base + self.history.len() as u64;
        loop {
            let num = self.next * self.in_rate;
            let floor = num / self.out_rate;
            // Taps cover real indices floor-half+1 ..= floor+half, which are
            // padded indices floor+1 ..= floor+2*half.
            if floor + taps as u64 >= end {
                break;
            }
            let frac = (num % self.out_rate) as f32 / self.out_rate as f32;
            let first = (floor + 1 - self.base) as usize;
            let mut acc = 0.0;
            for (j, &x) in self.history[first..first + taps].iter().enumerate() {
                let d = frac + (self.half as f32 - 1.0 - j as f32);
                acc += x * self.kernel(d);
            }
            out.push(acc);
            self.next += 1;
        }
        let needed = self.next * self.in_rate / self.out_rate + 1;
        let drop = (needed.saturating_sub(self.base) as usize).min(self.history.len());
        self.history.drain(..drop);
        self.base += drop as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
        let n = (f64::from(rate) * seconds).round() as usize;
        (0..n)
            .map(|i| (amplitude * (2.0 * PI * freq * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    /// Runs `input` through a resampler in blocks of `block` samples.
    fn resample(rate: u32, input: &[f32], block: usize) -> Vec<f32> {
        let mut r = Resampler::new(rate);
        let mut out = Vec::new();
        for piece in input.chunks(block) {
            r.process(piece, &mut out);
        }
        r.finish(&mut out);
        out
    }

    /// Frequency from the first and last rising zero crossings, at 16 kHz.
    fn frequency(samples: &[f32]) -> f64 {
        let mut crossings = Vec::new();
        for (i, pair) in samples.windows(2).enumerate() {
            let (a, b) = (f64::from(pair[0]), f64::from(pair[1]));
            if a < 0.0 && b >= 0.0 {
                crossings.push(i as f64 + a / (a - b));
            }
        }
        let (first, last) = (crossings[0], crossings[crossings.len() - 1]);
        (crossings.len() - 1) as f64 / ((last - first) / f64::from(SAMPLE_RATE))
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum: f64 = samples.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        (sum / samples.len() as f64).sqrt()
    }

    #[test]
    fn one_second_at_any_rate_becomes_16000_samples() {
        for rate in [
            8_000, 11_025, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000,
        ] {
            let input = vec![0.0; rate as usize];
            assert_eq!(resample(rate, &input, 441).len(), 16_000, "rate {rate}");
        }
    }

    #[test]
    fn output_length_rounds_up_the_exact_ratio() {
        // 1000 samples at 44.1 kHz are 362.8 output samples.
        assert_eq!(resample(44_100, &[0.0; 1000], 1000).len(), 363);
        assert_eq!(resample(48_000, &[0.0; 3], 3).len(), 1);
        assert_eq!(resample(48_000, &[], 3).len(), 0);
    }

    #[test]
    fn block_size_does_not_change_the_result() {
        let input = sine(44_100, 440.0, 0.5, 0.5);
        let whole = resample(44_100, &input, input.len());
        for block in [1, 7, 512, 4410] {
            let pieces = resample(44_100, &input, block);
            assert_eq!(pieces.len(), whole.len());
            for (a, b) in pieces.iter().zip(&whole) {
                assert!((a - b).abs() < 1e-6, "block {block}");
            }
        }
    }

    #[test]
    fn a_sine_keeps_its_frequency_and_level() {
        for (rate, freq) in [
            (48_000, 1000.0),
            (44_100, 1000.0),
            (8_000, 440.0),
            (96_000, 3000.0),
        ] {
            let out = resample(rate, &sine(rate, freq, 1.0, 0.5), 480);
            let middle = &out[2000..14_000];
            let measured = frequency(middle);
            assert!(
                (measured - freq).abs() < freq * 0.002,
                "rate {rate}: {measured} Hz"
            );
            let level = rms(middle);
            let expected = 0.5 / 2f64.sqrt();
            assert!(
                (level - expected).abs() < expected * 0.02,
                "rate {rate}: rms {level}"
            );
        }
    }

    #[test]
    fn tones_above_8khz_are_filtered_not_aliased() {
        let out = resample(48_000, &sine(48_000, 12_000.0, 1.0, 0.5), 480);
        assert!(rms(&out[2000..14_000]) < 0.005);
    }

    #[test]
    fn stereo_is_averaged_to_mono() {
        let left = sine(SAMPLE_RATE, 500.0, 0.1, 0.5);
        let opposite: Vec<f32> = left.iter().flat_map(|&x| [x, -x]).collect();
        let same: Vec<f32> = left.iter().flat_map(|&x| [x, x]).collect();
        let mut silent = Vec::new();
        let mut equal = Vec::new();
        let mut mono = Vec::new();
        let mut c = Converter::new(SAMPLE_RATE, 2, 1);
        c.push(&opposite, |x| x, |chunk| silent.extend(chunk.0));
        let mut c = Converter::new(SAMPLE_RATE, 2, 1);
        c.push(&same, |x| x, |chunk| equal.extend(chunk.0));
        let mut c = Converter::new(SAMPLE_RATE, 1, 1);
        c.push(&left, |x| x, |chunk| mono.extend(chunk.0));
        assert!(!silent.is_empty());
        assert!(silent.iter().all(|&b| b == 0));
        assert_eq!(equal, mono);
    }

    #[test]
    fn chunks_are_exact_and_16khz_mono_passes_through_unchanged() {
        let samples: Vec<i16> = (0..10_000)
            .map(|i| ((i * 37) % 65_536 - 32_768) as i16)
            .collect();
        let mut c = Converter::new(SAMPLE_RATE, 1, 1);
        let mut chunks = Vec::new();
        let mut offset = 0;
        for len in [1, 333, 640, 2, 5000].iter().cycle() {
            if offset >= samples.len() {
                break;
            }
            let end = (offset + len).min(samples.len());
            c.push(
                &samples[offset..end],
                |s| f32::from(s) / 32768.0,
                |chunk| chunks.push(chunk),
            );
            offset = end;
        }
        // 10000 samples are 20000 bytes: 15 full 1280-byte chunks, 800 pending.
        assert_eq!(chunks.len(), 15);
        c.finish(|chunk| chunks.push(chunk));
        assert_eq!(chunks.len(), 16);
        assert!(chunks.iter().all(|chunk| chunk.0.len() == CHUNK_BYTES));
        let bytes: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.0.clone()).collect();
        let expected: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(&bytes[..expected.len()], &expected[..]);
        assert!(bytes[expected.len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn finish_without_pending_audio_emits_nothing() {
        let mut c = Converter::new(SAMPLE_RATE, 1, 1);
        let mut count = 0;
        c.push(&[0i16; CHUNK_BYTES / 2], f32::from, |_| count += 1);
        c.finish(|_| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn gain_multiplies_and_saturates() {
        assert_eq!(amplify(1000, 3), 3000);
        assert_eq!(amplify(-1000, 3), -3000);
        assert_eq!(amplify(20_000, 2), i16::MAX);
        assert_eq!(amplify(-20_000, 2), i16::MIN);
        assert_eq!(amplify(i16::MIN, i32::MAX), i16::MIN);
        assert_eq!(amplify(1000, 1), 1000);
        assert_eq!(amplify(1000, 0), 1000);
        assert_eq!(amplify(1000, -5), 1000);
    }

    #[test]
    fn gain_applies_to_converted_chunks() {
        let mut c = Converter::new(SAMPLE_RATE, 1, 4);
        let mut out = Vec::new();
        c.push(
            &[1000i16; CHUNK_BYTES / 2],
            |s| f32::from(s) / 32768.0,
            |chunk| out = chunk.0,
        );
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 4000);
    }
}
