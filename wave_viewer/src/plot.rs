use anyhow::Result;
use clap::Parser;

use wave_viewer::args::Args;
use wave_viewer::decode::{Pack2In3BitsDecoder, READ_BUF_SIZE};
use wave_viewer::filter::DecimationFilter;
use wave_viewer::params::ParamsBuilder;
use wave_viewer::reader::spawn_reader;
use wave_viewer::viewer::ViewerAxisY;
use wave_viewer::viewer::plot::PlotApp;

fn main() -> Result<()> {
    let args = Args::parse();

    let mut builder = ParamsBuilder::new();
    builder.param0.set_value_range(1000, 0, 4095);
    let params = builder.build();
    let params_clone = params.clone();
    let filter = DecimationFilter::new();
    let decoder = Pack2In3BitsDecoder::new(filter);
    let y_axis = ViewerAxisY::new_with_autoscale();

    let rx = spawn_reader(
        decoder,
        READ_BUF_SIZE, 
        params_clone, 
        &args,
    )?;
    
    let native_options = eframe::NativeOptions::default();
    let app: PlotApp<u16> = PlotApp::new(rx, params, y_axis);
    eframe::run_native(
        "RP2350 ADC Viewer", 
        native_options, 
        Box::new(|_cc| Ok(Box::new(app)))
    ).expect("eframe::run_native failed");

    Ok(())
}

