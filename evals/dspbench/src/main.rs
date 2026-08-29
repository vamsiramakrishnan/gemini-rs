//! dspbench — effectiveness bench for the `voice::dsp` mic chain.
//!
//! Scores what the product experiences: VAD decisions against ground-truth
//! speech intervals (false activations/min, missed onsets, onset latency,
//! self-barge-in on echo-only scenes), with diagnostics underneath (ERLE,
//! segmental SNR improvement, log-spectral distortion, AGC level stats,
//! exit clipping). Deterministic: scenes are seeded; deltas between chain
//! variants are attributable to the chain.
//!
//!     cargo run --release --manifest-path evals/dspbench/Cargo.toml -- \
//!         [scenes.toml] [--out report.json]

mod metrics;
mod signal;

use std::collections::BTreeMap;
use std::path::PathBuf;

use gemini_adk_fluent_rs::voice::dsp::{
    aec::{Aec, AecConfig, AecFarEnd},
    stages::{Agc, HighPass, Limiter},
    DspChain, DspStage, IntStage,
};
use gemini_adk_fluent_rs::voice::Denoiser;
use gemini_adk_rs::live::InputAudioProcessor;
use gemini_genai_rs::vad::VadConfig;
use serde::{Deserialize, Serialize};

const SR: u32 = 16_000;
const FRAME: usize = 320; // 20 ms

// ── Scene definitions ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Manifest {
    scene: Vec<SceneDef>,
}

#[derive(Deserialize)]
struct SceneDef {
    name: String,
    duration_s: f32,
    seed: u64,
    speech: SpeechDef,
    noise: Option<NoiseDef>,
    echo: Option<EchoDef>,
    far: Option<SpeechDef>,
}

#[derive(Deserialize, Clone)]
struct SpeechDef {
    level_dbfs: f32,
    on_ms: u32,
    off_ms: u32,
}

#[derive(Deserialize)]
struct NoiseDef {
    kind: String,
    snr_db: f32,
}

#[derive(Deserialize)]
struct EchoDef {
    gain_db: f32,
    delay_ms: u32,
    taps: usize,
}

/// A materialized scene: what the mic hears, what the truth is.
struct Scene {
    name: String,
    mic: Vec<f32>,
    /// Clean near-end speech (SNR/LSD reference).
    clean: Vec<f32>,
    /// Ground-truth near-end speech intervals (samples).
    truth: Vec<(usize, usize)>,
    /// Far-end signal fed to the AEC reference input (bot playback).
    far: Option<Vec<f32>>,
    echo_delay_ms: u32,
    /// True when near end is silent: every activation is a self-barge-in.
    echo_only: bool,
}

fn build_scene(def: &SceneDef) -> Scene {
    let mut rng = signal::Rng::new(def.seed);
    let n = (def.duration_s * SR as f32) as usize;

    let (mut clean, truth) = if def.speech.on_ms == 0 {
        (vec![0.0; n], Vec::new())
    } else {
        signal::speech_proxy(
            &mut rng,
            SR,
            def.duration_s,
            def.speech.on_ms,
            def.speech.off_ms,
            def.speech.level_dbfs,
        )
    };
    clean.resize(n, 0.0);

    let mut mic = clean.clone();

    if let Some(noise) = &def.noise {
        let noise_rms = signal::rms(&clean_nonsilent(&clean, &truth))
            * 10f32.powf(-noise.snr_db / 20.0);
        let mut nz = match noise.kind.as_str() {
            "white" => signal::white(&mut rng, n, noise_rms),
            "pink" => signal::pink(&mut rng, n, noise_rms),
            "babble" => signal::babble(&mut rng, n, noise_rms),
            "traffic" => signal::traffic(&mut rng, n, noise_rms, SR),
            other => panic!("unknown noise kind {other}"),
        };
        nz.resize(n, 0.0);
        for (m, z) in mic.iter_mut().zip(&nz) {
            *m += z;
        }
    }

    let mut far_signal = None;
    let mut echo_delay_ms = 0;
    if let Some(echo) = &def.echo {
        let far_def = def.far.as_ref().expect("echo scene needs [far]");
        let (mut far, _) = signal::speech_proxy(
            &mut rng,
            SR,
            def.duration_s,
            far_def.on_ms,
            far_def.off_ms,
            far_def.level_dbfs,
        );
        far.resize(n, 0.0);
        let rir = signal::synth_rir(&mut rng, echo.taps, echo.gain_db);
        let echoed = signal::convolve(&far, &rir);
        let delay = (echo.delay_ms as usize * SR as usize) / 1000;
        for i in 0..n {
            if i >= delay && i - delay < echoed.len() {
                mic[i] += echoed[i - delay];
            }
        }
        far_signal = Some(far);
        echo_delay_ms = echo.delay_ms;
    }

    Scene {
        name: def.name.clone(),
        mic,
        clean,
        truth,
        far: far_signal,
        echo_delay_ms,
        echo_only: def.speech.on_ms == 0,
    }
}

/// Concatenated speech-active samples (level reference for SNR scaling).
fn clean_nonsilent(clean: &[f32], truth: &[(usize, usize)]) -> Vec<f32> {
    if truth.is_empty() {
        return clean.to_vec();
    }
    let mut out = Vec::new();
    for &(a, b) in truth {
        out.extend_from_slice(&clean[a.min(clean.len())..b.min(clean.len())]);
    }
    out
}

// ── Chain permutations ───────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Raw,
    Hpf,
    HpfDenoise,
    HpfAec,
    Full,
}

impl Variant {
    fn all() -> [Variant; 5] {
        [
            Variant::Raw,
            Variant::Hpf,
            Variant::HpfDenoise,
            Variant::HpfAec,
            Variant::Full,
        ]
    }
    fn name(self) -> &'static str {
        match self {
            Variant::Raw => "raw",
            Variant::Hpf => "hpf",
            Variant::HpfDenoise => "hpf+denoise",
            Variant::HpfAec => "hpf+aec",
            Variant::Full => "full",
        }
    }
    fn wants_aec(self) -> bool {
        matches!(self, Variant::HpfAec | Variant::Full)
    }
}

struct BuiltChain {
    aec: Option<(Aec, AecFarEnd)>,
    rest: DspChain,
}

fn build_chain(variant: Variant, echo_delay_ms: u32) -> BuiltChain {
    let aec = if variant.wants_aec() {
        Some(Aec::new(
            AecConfig {
                delay_ms: echo_delay_ms,
                ..AecConfig::default()
            },
            SR,
        ))
    } else {
        None
    };
    let mut rest = DspChain::new(SR);
    match variant {
        Variant::Raw => {}
        Variant::Hpf | Variant::HpfAec => {
            rest = rest.stage(HighPass::speech(SR));
        }
        Variant::HpfDenoise => {
            rest = rest
                .stage(HighPass::speech(SR))
                .stage(IntStage::named(Denoiser::new(SR), "denoise").with_latency(160));
        }
        Variant::Full => {
            rest = rest
                .stage(HighPass::speech(SR))
                .stage(IntStage::named(Denoiser::new(SR), "denoise").with_latency(160))
                .stage(Agc::speech_default(SR))
                .stage(Limiter::speech_default(SR));
        }
    }
    BuiltChain { aec, rest }
}

/// Run the scene's mic track through the chain, 20 ms frames, feeding the
/// AEC reference in lockstep. Returns (processed f32, total latency).
fn run_chain(scene: &Scene, chain: &mut BuiltChain) -> (Vec<f32>, usize) {
    let mut out = Vec::with_capacity(scene.mic.len());
    let mut frame_i16: Vec<i16> = Vec::with_capacity(FRAME);
    let mut float_buf: Vec<f32> = Vec::with_capacity(FRAME);

    for (idx, block) in scene.mic.chunks(FRAME).enumerate() {
        if let (Some((_, far_end)), Some(far)) = (&chain.aec, &scene.far) {
            let a = idx * FRAME;
            let b = (a + block.len()).min(far.len());
            if a < b {
                far_end.push_f32(&far[a..b]);
            }
        }
        float_buf.clear();
        float_buf.extend_from_slice(block);
        if let Some((aec, _)) = &mut chain.aec {
            let mut bus = gemini_adk_fluent_rs::voice::dsp::AudioBus {
                samples: &mut float_buf,
                sample_rate: SR,
            };
            aec.process(&mut bus);
        }
        frame_i16.clear();
        frame_i16.extend(
            float_buf
                .iter()
                .map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16),
        );
        chain.rest.process_frame(&mut frame_i16);
        out.extend(frame_i16.iter().map(|&s| f32::from(s) / 32768.0));
    }

    let latency = chain
        .aec
        .as_ref()
        .map(|(a, _)| a.latency_samples())
        .unwrap_or(0)
        + chain.rest.total_latency_samples();
    (out, latency)
}

// ── Report ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RunReport {
    decision: metrics::DecisionScore,
    seg_snr_db: Option<f32>,
    lsd_db: Option<f32>,
    level_mean_dbfs: f32,
    level_std_db: f32,
    erle_db: Option<f32>,
    latency_ms: f32,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let manifest_path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| {
            let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            p.push("scenes.toml");
            p.to_string_lossy().into_owned()
        });
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "dspbench-report.json".into());

    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(&manifest_path).expect("scenes.toml")).unwrap();

    let mut report: BTreeMap<String, BTreeMap<String, RunReport>> = BTreeMap::new();

    for def in &manifest.scene {
        let scene = build_scene(def);
        let mut per_variant = BTreeMap::new();
        for variant in Variant::all() {
            // AEC without a far end is a passthrough; skip to keep tables sharp.
            if variant.wants_aec() && scene.far.is_none() {
                continue;
            }
            let mut chain = build_chain(variant, scene.echo_delay_ms);
            let (processed, latency) = run_chain(&scene, &mut chain);

            let decision = metrics::score_vad(
                &processed,
                SR,
                &scene.truth,
                VadConfig::noisy_street(),
                latency,
                120,
            );
            let (seg_snr, lsd) = if scene.truth.is_empty() {
                (None, None)
            } else {
                (
                    Some(metrics::segmental_snr_db(
                        &processed,
                        &scene.clean,
                        SR,
                        &scene.truth,
                        latency,
                    )),
                    Some(metrics::log_spectral_distance_db(
                        &processed,
                        &scene.clean,
                        SR,
                        &scene.truth,
                        latency,
                    )),
                )
            };
            let (level_mean, level_std) = metrics::level_stats_dbfs(&processed, SR);
            let erle = chain.aec.as_ref().map(|(a, _)| a.erle_db());

            eprintln!(
                "{:>24} | {:<12} | falseAct/min {:>5.1} | missed {}/{} | onset p50 {:>6.0}ms | segSNR {} | ERLE {}",
                scene.name,
                variant.name(),
                decision.false_activations_per_min,
                decision.missed_onsets,
                decision.total_onsets,
                decision.onset_latency_ms_p50,
                seg_snr.map(|v| format!("{v:.1}dB")).unwrap_or_else(|| "-".into()),
                erle.map(|v| format!("{v:.1}dB")).unwrap_or_else(|| "-".into()),
            );

            per_variant.insert(
                variant.name().to_string(),
                RunReport {
                    decision,
                    seg_snr_db: seg_snr,
                    lsd_db: lsd,
                    level_mean_dbfs: level_mean,
                    level_std_db: level_std,
                    erle_db: erle,
                    latency_ms: latency as f32 * 1000.0 / SR as f32,
                },
            );
        }
        report.insert(scene.name.clone(), per_variant);
    }

    std::fs::write(&out_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("wrote {out_path}");
}
