use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    #[arg(long)]
    pub port: String,

    #[arg(long, default_value_t = 115200)]
    pub baud: u32,
}