#![no_std]
#![no_main]

use panic_halt as _;

use embedded_hal::digital::OutputPin;

use rp235x_hal as hal;

use hal::clocks::ClockSource;
use hal::gpio::FunctionPio0;
use hal::pio::PIOExt;
use hal::Sio;
use hal::watchdog::Watchdog;

use wave_example::music::{Note, Scale};

const XTAL_FREQ_HZ: u32 = 12_000_000;

const OUTPUT_HZ: u32 = 100_000;
const PROGRAM_STEP_HZ: u32 = OUTPUT_HZ * 2;
const DAC_MAX: u32 = 65535;

const BPM: u32 = 60;

#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

static SCORE: [Note; 40] = [
    Note{ scale: Scale::D4, div: 4, dot: false },
    Note{ scale: Scale::C4, div: 4, dot: false },
    Note{ scale: Scale::D4, div: 4, dot: false },
    Note{ scale: Scale::E4, div: 4, dot: false },

    Note{ scale: Scale::G4, div: 4, dot: false },
    Note{ scale: Scale::E4, div: 4, dot: false },
    Note{ scale: Scale::D4, div: 2, dot: false },

    Note{ scale: Scale::E4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 4, dot: false },
    Note{ scale: Scale::A4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 8, dot: false },
    Note{ scale: Scale::A4, div: 8, dot: false },

    Note{ scale: Scale::D5, div: 4, dot: false },
    Note{ scale: Scale::B4, div: 4, dot: false },
    Note{ scale: Scale::A4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 4, dot: false },

    Note{ scale: Scale::E4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 4, dot: false },
    Note{ scale: Scale::A4, div: 2, dot: false },

    Note{ scale: Scale::D5, div: 4, dot: false },
    Note{ scale: Scale::C5, div: 4, dot: false },
    Note{ scale: Scale::D5, div: 2, dot: false },

    Note{ scale: Scale::E4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 4, dot: false },
    Note{ scale: Scale::A4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 4, dot: false },

    Note{ scale: Scale::E4, div: 4, dot: true },
    Note{ scale: Scale::G4, div: 8, dot: false },
    Note{ scale: Scale::D4, div: 2, dot: false },

    Note{ scale: Scale::A4, div: 4, dot: false },
    Note{ scale: Scale::C5, div: 4, dot: false },
    Note{ scale: Scale::D5, div: 2, dot: false },

    Note{ scale: Scale::C5, div: 4, dot: false },
    Note{ scale: Scale::D5, div: 4, dot: false },
    Note{ scale: Scale::A4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 4, dot: false },

    Note{ scale: Scale::A4, div: 4, dot: false },
    Note{ scale: Scale::G4, div: 8, dot: false },
    Note{ scale: Scale::E4, div: 8, dot: false },
    Note{ scale: Scale::D4, div: 2, dot: false },
];

#[hal::entry]
fn main() -> ! {
    let mut p = hal::pac::Peripherals::take().unwrap();
    let mut watchdog = Watchdog::new(p.WATCHDOG);

    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        p.XOSC, 
        p.CLOCKS, 
        p.PLL_SYS, 
        p.PLL_USB, 
        &mut p.RESETS, 
        &mut watchdog
    ).ok().unwrap();
    let sys_hz = clocks.system_clock.get_freq().to_Hz();

    let sio = Sio::new(p.SIO);
    let pins = hal::gpio::Pins::new(
        p.IO_BANK0,
        p.PADS_BANK0,
        sio.gpio_bank0,
        &mut p.RESETS,
    );

    let d0 = pins.gpio0.into_function::<FunctionPio0>();
    let _d1 = pins.gpio1.into_function::<FunctionPio0>();
    let _d2 = pins.gpio2.into_function::<FunctionPio0>();
    let _d3 = pins.gpio3.into_function::<FunctionPio0>();
    let _d4 = pins.gpio4.into_function::<FunctionPio0>();
    let _d5 = pins.gpio5.into_function::<FunctionPio0>();
    let _d6 = pins.gpio6.into_function::<FunctionPio0>();
    let _d7 = pins.gpio7.into_function::<FunctionPio0>();
    let _d8 = pins.gpio8.into_function::<FunctionPio0>();
    let _d9 = pins.gpio9.into_function::<FunctionPio0>();
    let _d10 = pins.gpio10.into_function::<FunctionPio0>();
    let _d11 = pins.gpio11.into_function::<FunctionPio0>();
    let _d12 = pins.gpio12.into_function::<FunctionPio0>();
    let _d13 = pins.gpio13.into_function::<FunctionPio0>();
    let _d14 = pins.gpio14.into_function::<FunctionPio0>();
    let _d15 = pins.gpio15.into_function::<FunctionPio0>();

    let mut a = pio::Assembler::<32>::new();
    let mut wrap_target = a.label();
    let mut wrap_source = a.label();

    // 2 step
    a.bind(&mut wrap_target);
    a.pull(false, true);
    a.out(pio::OutDestination::PINS, 16);
    a.bind(&mut wrap_source);

    let program = a.assemble_with_wrap(wrap_source, wrap_target);

    let (mut pio0, sm0, _, _, _) = p.PIO0.split(&mut p.RESETS);
    let installed = pio0.install(&program).unwrap();

    let int = (sys_hz / PROGRAM_STEP_HZ) as u16;
    let rem = sys_hz % PROGRAM_STEP_HZ;
    let frac = ((rem * 256) / PROGRAM_STEP_HZ) as u8;
    let (mut sm, _, mut tx) = hal::pio::PIOBuilder::from_installed_program(installed)
        .out_pins(d0.id().num, 16)
        .clock_divisor_fixed_point(int, frac)
        .build(sm0);
    sm.set_pindirs((0..16 as u8).map(|pin| (pin, hal::pio::PinDir::Output)));
    sm.start();

    let mut led_pin = pins.gpio25.into_push_pull_output();
    let _ = led_pin.set_high().ok();

    loop {
        for note in SCORE.iter() {
            let hz = note.scale as u32;
            let div = note.div;
            let dot = note.dot;
            let steps = if dot {
                (OUTPUT_HZ * 60 * 4 * 3) / (BPM * div * 2)
            } else {
                (OUTPUT_HZ * 60 * 4)  / (BPM * div)
            };
            for count in 0..steps {
                while tx.is_full() {}
                let output = if ((count * 2 * hz) / OUTPUT_HZ) % 2 == 0 { DAC_MAX } else { 0 };
                tx.write(output);
            }
        }
    }
}