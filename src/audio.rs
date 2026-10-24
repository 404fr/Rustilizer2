use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rustfft::{num_complex::Complex, FftPlanner};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const FFT_SIZE: usize = 2048;

pub struct AudioAnalyzer {
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: Arc<Mutex<f32>>,
    planner: FftPlanner<f32>,
    window: Vec<f32>,
    fft_buffer: Vec<Complex<f32>>,
}

impl AudioAnalyzer {
    pub fn new() -> Self {
        let samples = Arc::new(Mutex::new(vec![0.0; FFT_SIZE]));
        let sample_rate = Arc::new(Mutex::new(48000.0));

        let samples_clone = Arc::clone(&samples);
        let rate_clone = Arc::clone(&sample_rate);

        // Background thread that maintains the WASAPI loopback stream
        thread::spawn(move || loop {
            if let Err(err) = run_capture_loop(&samples_clone, &rate_clone) {
                eprintln!("Audio capture error: {err}. Retrying in 2s...");
            }
            thread::sleep(Duration::from_secs(2));
        });

        // Precompute Hann window
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * (i as f32) / (FFT_SIZE as f32);
                0.5 * (1.0 - phase.cos())
            })
            .collect();

        Self {
            samples,
            sample_rate,
            planner: FftPlanner::new(),
            window,
            fft_buffer: vec![Complex::new(0.0, 0.0); FFT_SIZE],
        }
    }

    /// Computes logarithmically spaced frequency bars in [0.0, 1.0].
    pub fn compute_bars(&mut self, out_bars: &mut [f32]) {
        let num_bars = out_bars.len();
        if num_bars == 0 {
            return;
        }

        let rate = *self.sample_rate.lock().unwrap();
        {
            let ring = self.samples.lock().unwrap();
            for (i, (&s, &w)) in ring.iter().zip(self.window.iter()).enumerate() {
                self.fft_buffer[i] = Complex::new(s * w, 0.0);
            }
        }

        let fft = self.planner.plan_fft_forward(FFT_SIZE);
        fft.process(&mut self.fft_buffer);

        let half = FFT_SIZE / 2;
        let min_freq = 30.0_f32;
        let max_freq = 16000.0_f32.min(rate * 0.45);
        let log_min = min_freq.ln();
        let log_max = max_freq.ln();

        let norm = 1.0 / (FFT_SIZE as f32).sqrt();

        for (i, bar) in out_bars.iter_mut().enumerate() {
            let t0 = i as f32 / num_bars as f32;
            let t1 = (i + 1) as f32 / num_bars as f32;

            let f0 = (log_min + t0 * (log_max - log_min)).exp();
            let f1 = (log_min + t1 * (log_max - log_min)).exp();

            let bin0 = ((f0 * FFT_SIZE as f32 / rate).floor() as usize).clamp(1, half - 1);
            let bin1 = ((f1 * FFT_SIZE as f32 / rate).ceil() as usize).clamp(bin0 + 1, half);

            let mut sum = 0.0_f32;
            let mut max_val = 0.0_f32;
            for b in bin0..bin1 {
                let mag = self.fft_buffer[b].norm() * norm;
                sum += mag;
                if mag > max_val {
                    max_val = mag;
                }
            }
            let count = (bin1 - bin0).max(1) as f32;
            let avg = sum / count;
            let combined = avg * 0.6 + max_val * 0.4;

            // Frequency tilt compensation so higher frequencies appear balanced with bass
            let center_freq = 0.5 * (f0 + f1);
            let tilt = (center_freq / 120.0).powf(0.38).clamp(0.8, 4.5);
            let scaled = combined * tilt * 2.8;

            // Logarithmic dB-like curve mapped to 0.0..=1.0
            let db_mapped = ((1.0 + scaled * 18.0).ln() / (1.0_f32 + 18.0).ln()).clamp(0.0, 1.0);

            // Noise gate for silence
            *bar = if db_mapped < 0.03 {
                0.0
            } else {
                ((db_mapped - 0.03) / 0.97).powf(1.25).clamp(0.0, 1.0)
            };
        }
    }
}

fn run_capture_loop(
    samples: &Arc<Mutex<Vec<f32>>>,
    sample_rate: &Arc<Mutex<f32>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = cpal::default_host();

    // On Windows (WASAPI), opening the default output device as an input stream captures loopback audio
    let device = host
        .default_output_device()
        .or_else(|| host.default_input_device())
        .ok_or("No audio output or input device found")?;

    let supported_config = device
        .default_output_config()
        .or_else(|_| device.default_input_config())?;
    let sample_format = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    let channels = config.channels.max(1) as usize;

    *sample_rate.lock().unwrap() = config.sample_rate.0 as f32;

    let samples_ref = Arc::clone(samples);
    let err_fn = |err| eprintln!("Audio stream error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| push_samples(&samples_ref, data, channels, |s| s),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                push_samples(&samples_ref, data, channels, |s| s as f32 / i16::MAX as f32)
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                push_samples(&samples_ref, data, channels, |s| {
                    (s as f32 - 32768.0) / 32768.0
                })
            },
            err_fn,
            None,
        )?,
        _ => return Err("Unsupported sample format".into()),
    };

    stream.play()?;

    // Keep stream alive indefinitely
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn push_samples<T: Copy>(
    buffer: &Arc<Mutex<Vec<f32>>>,
    data: &[T],
    channels: usize,
    to_f32: impl Fn(T) -> f32,
) {
    if data.is_empty() {
        return;
    }

    let mut mono = Vec::with_capacity(data.len() / channels + 1);
    for frame in data.chunks(channels) {
        let mut sum = 0.0_f32;
        for &sample in frame {
            sum += to_f32(sample);
        }
        mono.push(sum / frame.len() as f32);
    }

    if let Ok(mut buf) = buffer.lock() {
        let len = buf.len();
        if mono.len() >= len {
            buf.copy_from_slice(&mono[mono.len() - len..]);
        } else {
            let shift = mono.len();
            buf.copy_within(shift..len, 0);
            buf[len - shift..].copy_from_slice(&mono);
        }
    }
}
