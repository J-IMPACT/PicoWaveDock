use anyhow::{Context, Result};
use rustfft::{FftPlanner, num_complex::Complex32};
use std::{
    collections::VecDeque,
    io::Read,
    sync::{
        atomic::Ordering,
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{args::Args, decode::search_port, params::ArcParams};

/// PDM bit rate
const FS_BITS_HZ: f64 = 3_750_000.0;
//const FS_BITS_HZ: f64 = 1_500_000.0;

/// CIC parameters (fixed, because goal is "reliably up to 200kHz")
/// fs_out = FS_BITS_HZ / R = 750 kHz
const CIC_R: u32 = 5;
const CIC_N: usize = 3;
const CIC_M: usize = 1;

/// FFT defaults
pub const FFT_N_DEFAULT: isize = 8192;
pub const AVG_FRAMES_DEFAULT: isize = 8;

/// FIR design defaults (compensate CIC droop + low-pass)
/// Keep passband flat up to 200kHz, transition to stop at 260kHz.
pub const F_PASS_HZ_DEFAULT: isize = 200_000;
pub const F_STOP_HZ_DEFAULT: isize = 260_000;
pub const FIR_TAPS_DEFAULT: isize = 63; // odd, "lightweight"

/// GUI <-> reader thread message
#[derive(Clone, Debug)]
pub struct SpectrumMsg {
    /// (freq_hz, db)
    pub points: Vec<(f32, f32)>,
    pub fs_out_hz: f64,
}

pub fn spawn_spectrum_reader(
    params: ArcParams,
    args: &Args
) -> Result<mpsc::Receiver<SpectrumMsg>> {
    search_port(args)?;

    let (tx, rx) = mpsc::channel::<SpectrumMsg>();
    let port_name = args.port.clone();
    let baud = args.baud;
    let params_clone = params.clone();

    thread::spawn(move || {
        if let Err(e) = reader_thread_cic_fir_fft(&port_name, baud, tx, params_clone) {
            eprintln!("[reader] error: {e:?}");
        }
    });

    Ok(rx)
}

fn reader_thread_cic_fir_fft(
    port_name: &str,
    baud: u32,
    tx: mpsc::Sender<SpectrumMsg>,
    params: ArcParams,
) -> Result<()> {
    let mut port = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(10))
        .open()
        .with_context(|| format!("failed to open port {port_name}"))?;
    let _ = port.clear(serialport::ClearBuffer::Input);

    let mut raw = [0u8; 64 * 1024];

    // CIC output sample rate
    let fs_out_hz = FS_BITS_HZ / (CIC_R as f64);

    // CIC decimator
    let mut cic = CicDecimator::new(CIC_R, CIC_N, CIC_M);
    let cic_gain = cic_gain(CIC_R, CIC_M, CIC_N);

    // FIR (designed from frequency sampling), state
    let mut fir: FirFilter = FirFilter::new(vec![1.0]);
    let mut fir_dirty = true;

    // FFT
    let mut planner = FftPlanner::<f32>::new();
    let mut current_fft_n: usize = 0;
    let mut fft = None::<std::sync::Arc<dyn rustfft::Fft<f32>>>;
    let mut buf_cpx: Vec<Complex32> = Vec::new();
    let mut window: Vec<f32> = Vec::new();

    // PSD averaging
    let mut psd_accum: Vec<f32> = Vec::new();
    let mut psd_count: u32 = 0;

    // PCM ring after CIC+FIR
    let mut pcm: VecDeque<f32> = VecDeque::with_capacity(1 << 18);

    let mut last_send = Instant::now();

    params.speed.store(fs_out_hz as usize, Ordering::Relaxed);

    while !params.stop.load(Ordering::Relaxed) {
        if params.paused.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        // read GUI params
        let n = (params.param0.load(Ordering::Relaxed) as u32).max(256) as usize;
        let avg = (params.param1.load(Ordering::Relaxed) as u32).max(1);
        let fpass = params.param2.load(Ordering::Relaxed) as f64;
        let fstop = params.param3.load(Ordering::Relaxed) as f64;
        let taps = ((params.param4.load(Ordering::Relaxed) as u32).max(7) as usize) | 1; // force odd

        // rebuild FFT plan if needed
        if n != current_fft_n {
            current_fft_n = n;
            fft = Some(planner.plan_fft_forward(n));
            buf_cpx = vec![Complex32::new(0.0, 0.0); n];
            window = hann_window(n);
            psd_accum = vec![0.0; n / 2 + 1];
            psd_count = 0;
        }

        // rebuild FIR if needed (params change)
        if fir_dirty
            || fir.len() != taps
            || fir.last_fpass_hz != fpass as u32
            || fir.last_fstop_hz != fstop as u32
        {
            // Design FIR: "CIC droop compensation + lowpass"
            // - passband: 0..fpass => magnitude = 1/|H_cic(f)| (clamped)
            // - transition: fpass..fstop => cosine taper to 0
            // - stopband: fstop..Nyq => 0
            let h = design_cic_comp_lowpass_fir(
                taps,
                fs_out_hz,          // FIR runs at fs_out
                FS_BITS_HZ,         // CIC droop computed at input fs
                CIC_R, CIC_M, CIC_N,
                fpass,
                fstop,
            );
            fir = FirFilter::new(h);
            fir.last_fpass_hz = fpass as u32;
            fir.last_fstop_hz = fstop as u32;

            // Flush filter state so phase settles quickly (optional)
            fir.reset();

            fir_dirty = false;

            pcm.clear();
        }

        // Receive bytes
        match port.read(&mut raw) {
            Ok(m) if m > 0 => {
                for &byte in &raw[..m] {
                    // bit order:
                    // ShiftDirection::Right で「古い→新しい」を作りたい場合、
                    // PC側は byte を bit7→bit0 の順に読むのが合わせやすい。
                    for bi in (0..8).rev() {
                        let b = ((byte >> bi) & 1) as i32;
                        let x = 2 * b - 1; // 0/1 -> -1/+1

                        if let Some(y_cic) = cic.push(x) {
                            // normalize roughly to ~[-1,+1]
                            let y = (y_cic as f32) / cic_gain;

                            // FIR (compensation + lowpass)
                            let y2 = fir.push(y);

                            pcm.push_back(y2);
                        }
                    }
                }
            }
            _ => {}
        }

        // FFT when enough samples
        if current_fft_n > 0 && pcm.len() >= current_fft_n {
            // Take a block (no overlap for simplicity)
            let mut block: Vec<f32> = Vec::with_capacity(current_fft_n);
            for _ in 0..current_fft_n {
                block.push(pcm.pop_front().unwrap());
            }

            // DC removal: subtract block mean
            let mean = block.iter().copied().sum::<f32>() / (current_fft_n as f32);
            for v in &mut block {
                *v -= mean;
            }

            // Window + complex
            for i in 0..current_fft_n {
                buf_cpx[i].re = block[i] * window[i];
                buf_cpx[i].im = 0.0;
            }

            // FFT
            if let Some(ref f) = fft {
                f.process(&mut buf_cpx);
            }

            // Power spectrum (one-sided)
            // (simple, relative scale)
            let mut psd: Vec<f32> = vec![0.0; current_fft_n / 2 + 1];
            for k in 0..=current_fft_n / 2 {
                psd[k] = buf_cpx[k].norm_sqr();
            }

            // accumulate average
            for k in 0..psd.len() {
                psd_accum[k] += psd[k];
            }
            psd_count += 1;

            if psd_count >= avg {
                let mut points: Vec<(f32, f32)> = Vec::with_capacity(psd_accum.len());

                for k in 0..psd_accum.len() {
                    let p = (psd_accum[k] / (psd_count as f32)).max(1e-20);
                    let db = 10.0 * p.log10();
                    let freq = (k as f64) * fs_out_hz / (current_fft_n as f64);
                    points.push((freq as f32, db));
                }

                // send at ~30fps max
                if last_send.elapsed() >= Duration::from_millis(33) {
                    let _ = tx.send(SpectrumMsg { points, fs_out_hz });
                    last_send = Instant::now();
                }

                // reset
                for v in &mut psd_accum { *v = 0.0; }
                psd_count = 0;
            }
        }
    }

    Ok(())
}

/* ===========================
   CIC decimator
   =========================== */

#[derive(Clone)]
struct CicDecimator {
    r: u32,
    n: usize,
    integ: Vec<i64>,
    comb_delay: Vec<VecDeque<i64>>,
    sample_count: u32,
}

impl CicDecimator {
    fn new(r: u32, n: usize, m: usize) -> Self {
        let mut comb_delay = Vec::with_capacity(n);
        for _ in 0..n {
            comb_delay.push(VecDeque::from(vec![0i64; m.max(1)]));
        }
        Self {
            r: r.max(1),
            n: n.max(1),
            integ: vec![0i64; n.max(1)],
            comb_delay,
            sample_count: 0,
        }
    }

    /// Push one input sample (±1). Output at decimated rate.
    fn push(&mut self, x: i32) -> Option<i64> {
        // integrators @ fs_in
        let mut v = x as i64;
        for s in 0..self.n {
            self.integ[s] = self.integ[s].wrapping_add(v);
            v = self.integ[s];
        }

        self.sample_count += 1;
        if self.sample_count < self.r {
            return None;
        }
        self.sample_count = 0;

        // combs @ fs_out
        let mut y = v;
        for s in 0..self.n {
            let d = self.comb_delay[s].pop_front().unwrap();
            let out = y.wrapping_sub(d);
            self.comb_delay[s].push_back(y);
            y = out;
        }
        Some(y)
    }
}

fn cic_gain(r: u32, m: usize, n: usize) -> f32 {
    (r as f32 * m as f32).powi(n as i32)
}

/// CIC magnitude (normalized to 1 at DC after dividing by gain).
/// Evaluate using input sampling rate fs_in (Hz).
fn cic_mag_norm(fs_in: f64, r: u32, m: usize, n: usize, f_hz: f64) -> f64 {
    if f_hz == 0.0 {
        return 1.0;
    }
    let rm = (r as f64) * (m as f64);
    let w = std::f64::consts::TAU * f_hz / fs_in; // rad/sample at fs_in
    let num = (0.5 * w * rm).sin();
    let den = (0.5 * w).sin();
    if den.abs() < 1e-18 {
        return 1.0;
    }
    let h1 = (num / (rm * den)).abs(); // 1-stage, DC-normalized
    h1.powi(n as i32)
}

/* ===========================
   FIR: droop-comp + lowpass design (frequency sampling)
   =========================== */

#[derive(Clone)]
struct FirFilter {
    h: Vec<f32>,
    z: VecDeque<f32>, // delay line
    last_fpass_hz: u32,
    last_fstop_hz: u32,
}

impl FirFilter {
    fn new(h: Vec<f32>) -> Self {
        let len = h.len().max(1);
        Self {
            h,
            z: VecDeque::from(vec![0.0; len]),
            last_fpass_hz: 0,
            last_fstop_hz: 0,
        }
    }

    fn len(&self) -> usize { self.h.len() }

    fn reset(&mut self) {
        self.z.clear();
        self.z.extend(std::iter::repeat(0.0).take(self.h.len()));
    }

    fn push(&mut self, x: f32) -> f32 {
        // z[0] oldest, push newest at back
        if self.z.len() >= self.h.len() {
            self.z.pop_front();
        }
        self.z.push_back(x);

        // dot(h, z) with linear phase taps
        // Align: h[0] multiplies oldest sample
        let mut acc = 0.0f32;
        for (hi, &zi) in self.h.iter().zip(self.z.iter()) {
            acc += hi * zi;
        }
        acc
    }
}

/// Design FIR via frequency sampling:
/// Desired response D(f):
/// - 0..fpass: 1/|H_cic(f)| (clamped)
/// - fpass..fstop: cosine taper to 0
/// - fstop..Nyq: 0
///
/// FIR runs at fs_out (Hz), but CIC droop is computed at fs_in.
fn design_cic_comp_lowpass_fir(
    taps: usize,
    fs_out: f64,
    fs_in: f64,
    r: u32,
    m: usize,
    n: usize,
    fpass: f64,
    fstop: f64,
) -> Vec<f32> {
    let taps = taps.max(3) | 1; // odd
    let nyq = fs_out * 0.5;
    let fpass = fpass.clamp(1.0, nyq * 0.98);
    let fstop = fstop.clamp(fpass + 1.0, nyq * 0.999);

    // Build dense frequency grid and IFFT to impulse, then take center taps.
    // Using Nfft sufficiently large improves tap quality.
    let nfft: usize = 4096; // fixed lightweight; can increase if needed
    let mut spec = vec![Complex32::new(0.0, 0.0); nfft];

    // Desired magnitude on [0..nyq]
    // We build Hermitian symmetry for real impulse response.
    for k in 0..=nfft / 2 {
        let f = (k as f64) * fs_out / (nfft as f64);

        let mag = if f <= fpass {
            // droop compensation
            let h = cic_mag_norm(fs_in, r, m, n, f);
            let inv = if h < 1e-6 { 1e6 } else { 1.0 / h };
            // clamp so filter doesn't blow up noise too much
            inv.min(8.0)
        } else if f < fstop {
            // cosine taper from 1 at fpass to 0 at fstop
            let t = (f - fpass) / (fstop - fpass);
            let w = 0.5 * (1.0 + (std::f64::consts::PI * t).cos()); // 1->0
            let h = cic_mag_norm(fs_in, r, m, n, f);
            let inv = if h < 1e-6 { 1e6 } else { 1.0 / h };
            (inv.min(8.0)) * w
        } else {
            0.0
        };

        spec[k] = Complex32::new(mag as f32, 0.0);
    }

    // Hermitian (k=0 and k=N/2 are real)
    for k in (nfft / 2 + 1)..nfft {
        let k2 = nfft - k;
        spec[k] = spec[k2].conj();
    }

    // IFFT to impulse
    let mut planner = FftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(nfft);
    ifft.process(&mut spec);

    // Normalize IFFT (rustfft doesn't)
    for v in &mut spec {
        v.re /= nfft as f32;
        v.im /= nfft as f32;
    }

    // Shift to linear-phase center and take taps around 0 (circular)
    // Real impulse is in spec[*].re
    let mid = nfft / 2;
    let half = taps / 2;

    let mut h = Vec::with_capacity(taps);
    for i in 0..taps {
        let idx = (mid + i - half) % nfft;
        h.push(spec[idx].re);
    }

    // Apply Hann window in time domain to reduce ripple
    let w = hann_window(taps);
    for i in 0..taps {
        h[i] *= w[i];
    }

    // Normalize gain at DC ~ 1
    let sum: f32 = h.iter().sum();
    if sum.abs() > 1e-9 {
        for v in &mut h {
            *v /= sum;
        }
    }

    h
}

/* ===========================
   FFT helpers
   =========================== */

fn hann_window(n: usize) -> Vec<f32> {
    if n <= 1 { return vec![1.0; n]; }
    let two_pi = std::f32::consts::TAU;
    (0..n).map(|i| {
        let x = two_pi * (i as f32) / ((n - 1) as f32);
        0.5 - 0.5 * x.cos()
    }).collect()
}