//! Diagnostic probe: Aec alone on a synthetic echo path, per-second stats.
#[path = "../src/signal.rs"]
#[allow(dead_code)]
mod signal;

use gemini_adk_fluent_rs::voice::dsp::{
    aec::{Aec, AecConfig},
    stages::HighPass,
    AudioBus, DspStage,
};

const SR: u32 = 16_000;
const FRAME: usize = 320;

fn main() {
    let mut rng = signal::Rng::new(16);
    let n = 20 * SR as usize;
    let (mut far, _) = signal::speech_proxy(&mut rng, SR, 20.0, 1600, 600, -18.0);
    far.resize(n, 0.0);
    let rir = signal::synth_rir(&mut rng, 48, -12.0);
    let echoed = signal::convolve(&far, &rir);
    let delay = 40 * SR as usize / 1000;
    let mut mic = vec![0.0f32; n];
    for i in delay..n {
        if i - delay < echoed.len() {
            mic[i] = echoed[i - delay];
        }
    }

    let (mut aec, far_end) = Aec::new(
        AecConfig {
            delay_ms: 40,
            mu: std::env::var("MU")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(AecConfig::default().mu),
            ..AecConfig::default()
        },
        SR,
    );

    let use_hpf = std::env::var("HPF").is_ok();
    let mut hpf = HighPass::speech(SR);
    let mut out = Vec::with_capacity(n);
    for (idx, block) in mic.chunks(FRAME).enumerate() {
        let a = idx * FRAME;
        let b = (a + block.len()).min(far.len());
        far_end.push_f32(&far[a..b]);
        let mut buf = block.to_vec();
        if use_hpf {
            let mut bus = AudioBus {
                samples: &mut buf,
                sample_rate: SR,
            };
            hpf.process(&mut bus);
        }
        let mut bus = AudioBus {
            samples: &mut buf,
            sample_rate: SR,
        };
        aec.process(&mut bus);
        out.extend_from_slice(&buf);
    }

    for sec in 0..20 {
        let a = sec * SR as usize;
        let b = a + SR as usize;
        let mic_rms = signal::rms(&mic[a..b.min(mic.len())]);
        let out_rms = signal::rms(&out[a..b.min(out.len())]);
        let far_rms = signal::rms(&far[a..b.min(far.len())]);
        println!(
            "t={sec:>2}s  far {far_rms:.4}  mic {mic_rms:.4}  out {out_rms:.4}  erle_now {:.1} dB",
            aec.erle_db()
        );
    }
}
