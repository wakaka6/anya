//! 16 kHz to 8 kHz, for models that take 8 kHz audio only.

/// Halves the sample rate of s16le mono PCM by averaging each pair of
/// samples, which also filters out most of what 8 kHz cannot hold. Bytes left
/// over at the end of one call (an odd sample, or half of one) are kept for
/// the next, so chunk boundaries do not matter.
#[derive(Debug, Default)]
pub(crate) struct Halve {
    carry: [u8; 4],
    len: usize,
}

impl Halve {
    /// The 8 kHz samples that `pcm`, after what earlier calls left over, completes.
    pub(crate) fn push(&mut self, mut pcm: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.len + pcm.len()) / 2);
        if self.len > 0 {
            let take = (4 - self.len).min(pcm.len());
            self.carry[self.len..self.len + take].copy_from_slice(&pcm[..take]);
            self.len += take;
            pcm = &pcm[take..];
            if self.len < 4 {
                return out;
            }
            average(&self.carry, &mut out);
            self.len = 0;
        }
        let mut pairs = pcm.chunks_exact(4);
        for pair in &mut pairs {
            average(pair, &mut out);
        }
        let rest = pairs.remainder();
        self.carry[..rest.len()].copy_from_slice(rest);
        self.len = rest.len();
        out
    }
}

fn average(pair: &[u8], out: &mut Vec<u8>) {
    let a = i32::from(i16::from_le_bytes([pair[0], pair[1]]));
    let b = i32::from(i16::from_le_bytes([pair[2], pair[3]]));
    // The mean of two i16 values is an i16.
    out.extend_from_slice(&(((a + b) / 2) as i16).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    #[test]
    fn halves_the_length_and_averages_pairs() {
        let out = Halve::default().push(&pcm(&[100, 300, -32768, -32768, 32767, 32767, -5, 6]));
        assert_eq!(samples(&out), [200, -32768, 32767, 0]);
    }

    #[test]
    fn a_tone_keeps_its_shape() {
        // 200 Hz at 16 kHz: one second in 40 ms chunks.
        let tone: Vec<i16> = (0..16_000)
            .map(|i| {
                let t = f64::from(i) / 16_000.0;
                (10_000.0 * (2.0 * std::f64::consts::PI * 200.0 * t).sin()) as i16
            })
            .collect();
        let mut halve = Halve::default();
        let out: Vec<u8> = pcm(&tone)
            .chunks(1280)
            .flat_map(|c| halve.push(c))
            .collect();
        let out = samples(&out);
        assert_eq!(out.len(), 8_000);
        for (i, s) in out.iter().enumerate() {
            // Each output sample sits between the two it averages.
            let t = (2.0 * i as f64 + 0.5) / 16_000.0;
            let want = 10_000.0 * (2.0 * std::f64::consts::PI * 200.0 * t).sin();
            assert!(
                (f64::from(*s) - want).abs() < 20.0,
                "sample {i}: {s} vs {want}"
            );
        }
    }

    #[test]
    fn chunk_boundaries_do_not_matter() {
        let input = pcm(&(0..999)
            .map(|i| (i * 37 % 2000 - 1000) as i16)
            .collect::<Vec<_>>());
        let whole = Halve::default().push(&input);
        for size in [1, 3, 5, 7, 1280] {
            let mut halve = Halve::default();
            let split: Vec<u8> = input.chunks(size).flat_map(|c| halve.push(c)).collect();
            assert_eq!(split, whole, "chunks of {size}");
        }
        assert_eq!(whole.len(), 499 * 2);
    }
}
