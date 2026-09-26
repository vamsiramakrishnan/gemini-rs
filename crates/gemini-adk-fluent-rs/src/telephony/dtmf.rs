//! In-band DTMF: keypad tones found in the call audio itself.
//!
//! Carriers deliver keypresses in two ways. Out of band, as RFC 4733
//! telephone-events or a platform's `dtmf` message; those are handled by the
//! SIP and Twilio bridges. Or in band, as the dual tones themselves inside
//! the audio: a SIP trunk without telephone-events, a transferred call, or a
//! caller pressing keys while the platform forwards raw media.
//!
//! [`DtmfDetector`] finds in-band tones with the Goertzel algorithm over the
//! eight DTMF frequencies, in blocks of 25.6 ms (205 samples at 8 kHz), with
//! the checks ITU-T Q.24 receivers use: both a row and a column tone must be
//! strong, dominate their groups, sit within 8 dB of each other, and carry
//! most of the block's energy, which is what keeps speech and music from
//! registering as digits.

const ROWS: [f64; 4] = [697.0, 770.0, 852.0, 941.0];
const COLS: [f64; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
const KEYS: [[char; 4]; 4] = [
    ['1', '2', '3', 'A'],
    ['4', '5', '6', 'B'],
    ['7', '8', '9', 'C'],
    ['*', '0', '#', 'D'],
];
/// Block length in seconds (205 samples at 8 kHz).
const BLOCK_SECS: f64 = 0.025_625;
/// 8 dB as a power ratio: the most the row and column tones may differ
/// (twist), and the least by which each must beat the rest of its group.
const EIGHT_DB: f64 = 6.31;
/// Share of the block's energy the two tones must carry.
const PURITY: f64 = 0.6;
/// Quietest tone accepted: RMS of about -36 dBFS.
const MIN_MEAN_SQUARE: f64 = 500.0 * 500.0;

/// Streaming in-band DTMF detector. See the module docs.
#[derive(Debug, Clone)]
pub struct DtmfDetector {
    block: usize,
    coeffs: [f64; 8],
    buffer: Vec<i16>,
    /// The digit the last complete block held, for edge detection.
    last: Option<char>,
}

impl DtmfDetector {
    /// A detector for mono PCM16 at `sample_rate` (8 kHz on the phone
    /// network; any rate of 4 kHz or more works).
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(4_000));
        let block = (rate * BLOCK_SECS).round() as usize;
        let mut coeffs = [0.0; 8];
        for (i, f) in ROWS.iter().chain(COLS.iter()).enumerate() {
            let k = (block as f64 * f / rate).round();
            coeffs[i] = 2.0 * (2.0 * std::f64::consts::PI * k / block as f64).cos();
        }
        Self {
            block,
            coeffs,
            buffer: Vec::with_capacity(block),
            last: None,
        }
    }

    /// Feed audio; returns each keypress that began in it, in order. A held
    /// key is reported once.
    pub fn feed(&mut self, samples: &[i16]) -> Vec<char> {
        let mut digits = Vec::new();
        for &sample in samples {
            self.buffer.push(sample);
            if self.buffer.len() == self.block {
                let digit = self.classify();
                if digit.is_some() && digit != self.last {
                    digits.extend(digit);
                }
                self.last = digit;
                self.buffer.clear();
            }
        }
        digits
    }

    /// Whether the last complete block held a keypad tone.
    pub fn tone_present(&self) -> bool {
        self.last.is_some()
    }

    fn classify(&self) -> Option<char> {
        let n = self.buffer.len() as f64;
        let energy: f64 = self.buffer.iter().map(|&s| f64::from(s).powi(2)).sum();
        if energy / n < MIN_MEAN_SQUARE {
            return None;
        }
        let mut power = [0f64; 8];
        for (i, &coeff) in self.coeffs.iter().enumerate() {
            let (mut s1, mut s2) = (0f64, 0f64);
            for &x in &self.buffer {
                let s0 = f64::from(x) + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            power[i] = s1 * s1 + s2 * s2 - coeff * s1 * s2;
        }
        let strongest = |group: &[f64]| {
            let (best, &p) = group
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .expect("four tones");
            let runner_up = group
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != best)
                .map(|(_, &q)| q)
                .fold(0.0, f64::max);
            (best, p, runner_up)
        };
        let (row, row_power, row_next) = strongest(&power[..4]);
        let (col, col_power, col_next) = strongest(&power[4..]);
        let dominant = row_power > row_next * EIGHT_DB && col_power > col_next * EIGHT_DB;
        let twist_ok = row_power < col_power * EIGHT_DB && col_power < row_power * EIGHT_DB;
        // A Goertzel power of P for a full-block sine means P * 2 / n of
        // that sine's energy over the block.
        let tone_energy = (row_power + col_power) * 2.0 / n;
        let pure = tone_energy > PURITY * energy;
        (dominant && twist_ok && pure).then_some(KEYS[row][col])
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `digits` as tones of `on_ms`, separated by `off_ms` of silence, at 8 kHz.
    pub(crate) fn keypresses(digits: &str, on_ms: usize, off_ms: usize, level: f64) -> Vec<i16> {
        let rate = 8_000.0;
        let mut out = Vec::new();
        for digit in digits.chars() {
            let (r, c) = KEYS
                .iter()
                .enumerate()
                .find_map(|(r, row)| row.iter().position(|&k| k == digit).map(|c| (r, c)))
                .expect("a keypad digit");
            for n in 0..on_ms * 8 {
                let t = n as f64 / rate;
                let v = level
                    * ((2.0 * std::f64::consts::PI * ROWS[r] * t).sin()
                        + (2.0 * std::f64::consts::PI * COLS[c] * t).sin());
                out.push(v as i16);
            }
            out.extend(std::iter::repeat_n(0, off_ms * 8));
        }
        out
    }

    #[test]
    fn keypresses_are_detected_once_each() {
        let audio = keypresses("4129#*0D", 70, 50, 6_000.0);
        let mut detector = DtmfDetector::new(8_000);
        let mut found = String::new();
        // In 20 ms frames, as a phone bridge delivers them.
        for frame in audio.chunks(160) {
            found.extend(detector.feed(frame));
        }
        assert_eq!(found, "4129#*0D");
    }

    #[test]
    fn a_held_key_is_one_press_and_repeats_need_a_gap() {
        let mut detector = DtmfDetector::new(8_000);
        let held = keypresses("5", 400, 60, 6_000.0);
        assert_eq!(detector.feed(&held), ['5']);
        let again = keypresses("55", 60, 60, 6_000.0);
        assert_eq!(detector.feed(&again), ['5', '5']);
    }

    #[test]
    fn speech_like_audio_is_not_a_digit() {
        // A voiced sound: a 140 Hz fundamental with falling harmonics that
        // pass through the DTMF bands, plus noise.
        let mut seed = 7u32;
        let audio: Vec<i16> = (0..8_000 * 2)
            .map(|n| {
                let t = n as f64 / 8_000.0;
                let voiced: f64 = (1..=25)
                    .map(|h| {
                        (4_000.0 / h as f64)
                            * (2.0 * std::f64::consts::PI * 140.0 * h as f64 * t).sin()
                    })
                    .sum();
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = f64::from((seed >> 16) as i16 >> 4);
                (voiced + noise) as i16
            })
            .collect();
        let mut detector = DtmfDetector::new(8_000);
        assert!(detector.feed(&audio).is_empty());
    }

    #[test]
    fn quiet_or_single_tones_are_ignored() {
        let mut detector = DtmfDetector::new(8_000);
        assert!(detector.feed(&keypresses("7", 100, 0, 200.0)).is_empty());
        // A lone 1 kHz tone (a dial tone or a beep) is not a keypress.
        let beep: Vec<i16> = (0..1_600)
            .map(|n| {
                (8_000.0 * (2.0 * std::f64::consts::PI * 1_000.0 * n as f64 / 8_000.0).sin()) as i16
            })
            .collect();
        assert!(detector.feed(&beep).is_empty());
    }
}
