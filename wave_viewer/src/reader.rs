use anyhow::Result;
use std::sync::mpsc;
use std::thread;

use crate::args::Args;
use crate::data::Data;
use crate::decode::{Decoder, search_port};
use crate::filter::Filter;
use crate::params::ArcParams;

pub fn spawn_reader<D, F, TI, TO>(
    mut decoder: D,
    write_buf_size: usize,
    params: ArcParams,
    args: &Args
) -> Result<mpsc::Receiver<Vec<TO>>> 
where
    D: Decoder<F, TO> + 'static,
    F: Filter<TI, TO>,
    TI: Data,
    TO: Data + 'static
{
    search_port(args)?;

    let (tx, rx) = mpsc::channel::<Vec<TO>>();

    let port_name = args.port.clone();
    let baud = args.baud;

    thread::spawn(move || {
        if let Err(e) = decoder.reader_thread(
            tx,
            &port_name, 
            baud,
            write_buf_size,
            params
        ) {
            eprintln!("[reader] error: {e:?}");
        }
    });

    Ok(rx)
}