use anyhow::Result;
use clap::Parser;

use wave_viewer::args::Args;
use wave_viewer::dsp::fft::{
    AVG_FRAMES_DEFAULT, FFT_N_DEFAULT, FIR_TAPS_DEFAULT, F_PASS_HZ_DEFAULT,
    F_STOP_HZ_DEFAULT, spawn_spectrum_reader,
};
use wave_viewer::params::ParamsBuilder;
use wave_viewer::viewer::spectrum::SpectrumApp;

fn main() -> Result<()> {
    let args = Args::parse();

    let mut builder = ParamsBuilder::new();
    builder.param0.set_value_range(FFT_N_DEFAULT, 256, 32768);
    builder.param1.set_value_range(AVG_FRAMES_DEFAULT, 1, 64);
    builder.param2.set_value_range(F_PASS_HZ_DEFAULT, 10_000, 300_000);
    builder.param3.set_value_range(F_STOP_HZ_DEFAULT, 20_000, 370_000);
    builder.param4.set_value_range(FIR_TAPS_DEFAULT, 7, 255);
    let params = builder.build();

    let rx = spawn_spectrum_reader(params.clone(), &args)?;

    let native_options = eframe::NativeOptions::default();
    let app = SpectrumApp::new(rx, params);

    eframe::run_native(
        "RP2350 PDM Spectrum (CIC+FIR)", 
        native_options, 
        Box::new(|_cc| Ok(Box::new(app)))
    ).expect("eframe::run_native failed");
    Ok(())
}