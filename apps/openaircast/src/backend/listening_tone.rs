//! Finite, low-level PCM signal used by the receiver listening check.

/// Duration of the listening tone.
pub(crate) const LISTENING_TONE_DURATION_SECS: usize = 2;
/// Frequency chosen to remain clearly audible on small speakers.
const LISTENING_TONE_HZ: f64 = 660.0;
/// Conservative peak level for a diagnostic signal.
const LISTENING_TONE_AMPLITUDE: f64 = 8_192.0;
/// Linear fade length at either edge, avoiding an audible click.
const LISTENING_TONE_FADE_SAMPLES: usize = 1_103; // approximately 25 ms at 44.1 kHz

/// Generates one finite stereo listening tone in the capture pipeline format.
pub(crate) fn generate_listening_tone() -> Vec<i16> {
    let sample_rate = usize::try_from(super::capture::CAPTURE_SAMPLE_RATE)
        .expect("the capture sample rate fits usize");
    let channels = usize::from(super::capture::CAPTURE_CHANNELS);
    let sample_frames = sample_rate * LISTENING_TONE_DURATION_SECS;
    let mut samples = Vec::with_capacity(sample_frames * channels);

    for index in 0..sample_frames {
        let fade_in = index as f64 / LISTENING_TONE_FADE_SAMPLES as f64;
        let fade_out = (sample_frames - 1 - index) as f64 / LISTENING_TONE_FADE_SAMPLES as f64;
        let envelope = fade_in.min(fade_out).min(1.0);
        let phase = std::f64::consts::TAU * LISTENING_TONE_HZ * index as f64 / sample_rate as f64;
        let sample = (phase.sin() * LISTENING_TONE_AMPLITUDE * envelope).round() as i16;
        samples.extend(std::iter::repeat_n(sample, channels));
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: usize = 44_100;
    const CHANNELS: usize = 2;

    #[test]
    fn generated_tone_is_finite_bounded_stereo_and_near_660_hz() {
        let samples = generate_listening_tone();

        assert_eq!(samples.len(), SAMPLE_RATE * CHANNELS * 2);
        assert!(samples.iter().any(|sample| *sample != 0));
        assert!(samples.iter().all(|sample| sample.unsigned_abs() <= 8_192));
        assert!(samples.chunks_exact(2).all(|pair| pair[0] == pair[1]));

        let positive_crossings = samples
            .chunks_exact(2)
            .map(|pair| pair[0])
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|pair| pair[0] <= 0 && pair[1] > 0)
            .count();
        assert!(
            (1_318..=1_322).contains(&positive_crossings),
            "two seconds at about 660 Hz should have about 1,320 positive crossings, got {positive_crossings}"
        );
    }

    #[test]
    fn generated_tone_fades_in_and_out() {
        let samples = generate_listening_tone();
        let left = samples
            .chunks_exact(2)
            .map(|pair| pair[0])
            .collect::<Vec<_>>();
        let peak = |range: std::ops::Range<usize>| {
            left[range]
                .iter()
                .map(|sample| sample.unsigned_abs())
                .max()
                .unwrap()
        };

        assert_eq!(left[0], 0);
        assert_eq!(left[SAMPLE_RATE * 2 - 1], 0);
        assert!(peak(0..220) < 2_000, "the first 5 ms must be faded in");
        assert!(
            peak(SAMPLE_RATE - 220..SAMPLE_RATE + 220) > 7_500,
            "the middle of the tone must reach its useful listening level"
        );
        assert!(
            peak(SAMPLE_RATE * 2 - 220..SAMPLE_RATE * 2) < 2_000,
            "the final 5 ms must be faded out"
        );
    }
}
