use anyhow::{bail, Context, Result};

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::data::Data;
use crate::filter::Filter;
use crate::params::ArcParams;
use crate::speed::SpeedMeter;

pub const READ_BUF_SIZE: usize = 32768;

pub fn search_port(args: &Args) -> Result<()> {
    let ports = serialport::available_ports().context("serialport::available_ports failed")?;
    let exists = ports.iter().any(|p| p.port_name == args.port);
    if !exists {
        let names: Vec<_> = ports.into_iter().map(|p| p.port_name).collect();
        bail!(
            "Port Not Found: {}\nAvailable: {:?}",
            args.port,
            names
        );
    }
    Ok(())
}

pub trait Decoder<F, TO: Data>: Send {
    fn reader(
        &mut self,
        read_buf: &[u8],
        write_buf: &mut Vec<TO>,
        params: ArcParams
    ) -> Result<()>;
    fn reader_thread(
        &mut self,
        tx: mpsc::Sender<Vec<TO>>,
        port_name: &str,
        baud: u32,
        write_buf_size: usize,
        params: ArcParams
    ) -> Result<()> {
        let mut port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(10))
            .open()
            .with_context(|| format!("failed to open port {port_name}"))?;

        let _ = port.clear(serialport::ClearBuffer::Input);

        let mut read_buf = [0u8; READ_BUF_SIZE];
        let mut write_buf: Vec<TO> = Vec::with_capacity(write_buf_size);

        let mut last_flush = Instant::now();

        while !params.stop.load(Ordering::Relaxed) {
            if params.paused.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            match port.read(&mut read_buf) {
                Ok(n) if n > 0 => self.reader(
                    &read_buf[..n],
                    &mut write_buf,
                    params.clone()
                )?,
                _ => {}
            }

            if write_buf.len() >= 8192 || last_flush.elapsed() >= Duration::from_millis(8) {
                if !write_buf.is_empty() {
                    if tx.send(std::mem::take(&mut write_buf)).is_err() {
                        write_buf.clear();
                    }
                    last_flush = Instant::now();
                }
            }
        }

        Ok(())
    }
}

pub struct LsbBitsDecoder<F> {
    filter: F,
    speed_meter: SpeedMeter,
}

impl<F> LsbBitsDecoder<F> {
    pub fn new(filter: F) -> Self {
        let speed_meter = SpeedMeter::new(8, 1);
        Self { filter, speed_meter, }
    }
}

impl<F: Filter<u8, TO>, TO: Data> Decoder<F, TO> for LsbBitsDecoder<F> {
    fn reader(
        &mut self,
        read_buf: &[u8],
        write_buf: &mut Vec<TO>,
        params: ArcParams
    ) -> Result<()> {
        self.speed_meter.run(read_buf.len(), params.clone());

        for &b in read_buf {
            for i in 0..8 {
                if let Some(out) = self.filter.run((b >> i) & 1, params.clone()) {
                    write_buf.push(out);
                }
            }
        }
        Ok(())
    }
}

pub struct Pack2In3BitsDecoder<F> {
    filter: F,
    speed_meter: SpeedMeter,
    phase: Option<usize>,
    keep_queue: VecDeque<u8>,
}

impl<F> Pack2In3BitsDecoder<F> {
    pub fn new(filter: F) -> Self {
        let speed_meter = SpeedMeter::new(2, 3);
        let keep_queue = VecDeque::with_capacity(32_768);
        Self { filter, speed_meter, phase: None, keep_queue, }
    }
}

impl<F: Filter<u16, TO>, TO: Data> Decoder<F, TO> for Pack2In3BitsDecoder<F> {
    fn reader(
        &mut self,
        read_buf: &[u8],
        write_buf: &mut Vec<TO>,
        params: ArcParams
    ) -> Result<()> {
        self.keep_queue.extend(read_buf);

        if self.phase.is_none() {
            if let Some(p) = estimate_phase(&self.keep_queue) {
                self.phase = Some(p);
                for _ in 0..p {
                    self.keep_queue.pop_front();
                }
            } else {
                return Ok(());
            }
        }

        self.speed_meter.run(read_buf.len(), params.clone());

        while self.keep_queue.len() >= 3 {
            let b0 = self.keep_queue.pop_front().unwrap();
            let b1 = self.keep_queue.pop_front().unwrap();
            let b2 = self.keep_queue.pop_front().unwrap();

            let (s0, s1) = unpack_3bytes_to_2x12bits(b0, b1, b2);

            for v in [s0, s1] {
                if let Some(out) = self.filter.run(v, params.clone()) {
                    write_buf.push(out);
                }
            }
        }
        Ok(())
    }
}

#[inline(always)]
pub fn unpack_3bytes_to_2x12bits(b0: u8, b1: u8, b2: u8) -> (u16, u16) {
    let s0 = (b0 as u16) | (((b1 as u16) & 0x0F) << 8);
    let s1 = (((b1 as u16) >> 4) & 0x0F) | ((b2 as u16) << 4);
    (s0, s1)
}

/// Estimate phase (0/1/2) for 3-byte framing using smoothness heuristic.
/// Works well for non-white signals. For pure noise, it may be ambiguous.
pub fn estimate_phase(buf: &VecDeque<u8>) -> Option<usize> {
    if buf.len() < 3 * 512 + 2 {
        return None;
    }

    const PAIRS: usize = 512;
    let mut best_phase = 0usize;
    let mut best_score: u64 = u64::MAX;

    for phase in 0..3 {
        let mut i = phase;
        let mut prev: Option<u16> = None;
        let mut score: u64 = 0;

        for _ in 0..PAIRS {
            let b0 = buf[i];
            let b1 = buf[i + 1];
            let b2 = buf[i + 2];
            let (s0, s1) = unpack_3bytes_to_2x12bits(b0, b1, b2);

            if let Some(p) = prev {
                score += (s0 as i32 - p as i32).abs() as u64;
            }
            score += (s1 as i32 - s0 as i32).abs() as u64;
            prev = Some(s1);

            i += 3;
        }

        if score < best_score {
            best_score = score;
            best_phase = phase;
        }
    }

    Some(best_phase)
}