#![no_std]
#![no_main]

use panic_halt as _;
use rp235x_hal::sio::{Lane, LaneCtrl};

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

use embedded_hal::digital::OutputPin;
use heapless::String;

use rp235x_hal as hal;

use hal::clocks::ClockSource;
use rp235x_hal::dma::{DMAExt, double_buffer};
use hal::gpio::{bank0, FunctionPio0, Pin, PullDown};
use hal::multicore::{Multicore, Stack};
use hal::pio::{InstalledProgram, PIOExt};
use hal::Sio;
use hal::watchdog::Watchdog;

use usb_device::{class_prelude::*, prelude::*};
use usbd_serial::SerialPort;

use wave_example::tables::SINE_LUT;

const XTAL_FREQ_HZ: u32 = 12_000_000;

const OUTPUT_HZ: u32 = 2_000_000;
const PROGRAM_STEP_HZ: u32 = OUTPUT_HZ * 2;

const LUT_BITS: u32 = 12;
const LUT_LEN: usize = 1 << LUT_BITS;

const DDS_HZ_DEFAULT: u32 = 1_000;

const DDS_BUF_SAMPLES: usize = 1024;

static CORE1_STACK: Stack<4096> = Stack::new();

static DDS_FREQ: AtomicU32 = AtomicU32::new(DDS_HZ_DEFAULT);

#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

struct DdsPool(UnsafeCell<[[u32; DDS_BUF_SAMPLES]; 2]>);
unsafe impl Sync for DdsPool {}
static DDS_POOL: DdsPool = DdsPool(UnsafeCell::new([[0u32; DDS_BUF_SAMPLES]; 2]));

#[inline(always)]
unsafe fn dds_buf_mut(i: usize) -> &'static mut [u32; DDS_BUF_SAMPLES] {
    unsafe { &mut (*DDS_POOL.0.get())[i] }
}

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

    let mut sio = Sio::new(p.SIO);

    let pins = hal::gpio::Pins::new(
        p.IO_BANK0,
        p.PADS_BANK0,
        sio.gpio_bank0,
        &mut p.RESETS
    );

    let pio_pins = (
        pins.gpio0.into_function::<FunctionPio0>(),
        pins.gpio1.into_function::<FunctionPio0>(),
        pins.gpio2.into_function::<FunctionPio0>(),
        pins.gpio3.into_function::<FunctionPio0>(),
        pins.gpio4.into_function::<FunctionPio0>(),
        pins.gpio5.into_function::<FunctionPio0>(),
        pins.gpio6.into_function::<FunctionPio0>(),
        pins.gpio7.into_function::<FunctionPio0>(),
        pins.gpio8.into_function::<FunctionPio0>(),
        pins.gpio9.into_function::<FunctionPio0>(),
        pins.gpio10.into_function::<FunctionPio0>(),
        pins.gpio11.into_function::<FunctionPio0>(),
        pins.gpio12.into_function::<FunctionPio0>(),
        pins.gpio13.into_function::<FunctionPio0>(),
        pins.gpio14.into_function::<FunctionPio0>(),
        pins.gpio15.into_function::<FunctionPio0>(),
    );

    let mut a = pio::Assembler::<32>::new();
    let mut wrap_target = a.label();
    let mut wrap_source = a.label();

    a.bind(&mut wrap_target);
    a.pull(false, true);
    a.out(pio::OutDestination::PINS, 16);
    a.bind(&mut wrap_source);

    let program = a.assemble_with_wrap(wrap_source, wrap_target);

    let (mut pio0, sm0, _, _, _) = p.PIO0.split(&mut p.RESETS);
    let installed = pio0.install(&program).unwrap();

    let mut dch = p.DMA.dyn_split(&mut p.RESETS);

    let pio_ch2 = dch.ch2.take().unwrap();
    let pio_ch3 = dch.ch3.take().unwrap();

    let mut mc = Multicore::new(&mut p.PSM, &mut p.PPB, &mut sio.fifo);
    let cores = mc.cores();
    let core1 = &mut cores[1];

    let interp0 = sio.interp0;

    core1.spawn(CORE1_STACK.take().unwrap(), move || {
        core1_dds_pio_dma_task(
            sm0,
            installed,
            pio_pins,
            sys_hz,
            interp0,
            pio_ch2,
            pio_ch3
        )
    }).unwrap();

    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        p.USB,
        p.USB_DPRAM,
        clocks.usb_clock,
        true,
        &mut p.RESETS
    ));

    let mut serial = SerialPort::new(&usb_bus);

    let mut usb_dev = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x2E8A, 0x000A))
        .device_class(usbd_serial::USB_CLASS_CDC)
        .max_packet_size_0(64).unwrap()
        .strings(&[
            StringDescriptors::new(LangID::EN_US)
                .manufacturer("Example")
                .product("DDS Generator")
                .serial_number("0001")
        ]).unwrap()
        .build();

    let mut rx_buf = [0u8; 64];
    let mut cmd = String::<16>::new();

    let mut configured = false;

    let mut led_pin = pins.gpio25.into_push_pull_output();
    let _ = led_pin.set_high();

    loop {
        if !usb_dev.poll(&mut [&mut serial]) {
            continue;
        }

        if usb_dev.state() == UsbDeviceState::Configured && !configured {

            configured = true;

            let f = DDS_FREQ.load(Ordering::Relaxed);

            let mut buf = String::<32>::new();
            let _ = core::fmt::write(&mut buf, format_args!("Freq: {}\r\n> ", f));

            usb_write(&mut serial, &buf);
        }

        if let Ok(count) = serial.read(&mut rx_buf) {

            for &b in &rx_buf[..count] {

                match b {

                    b'\r' | b'\n' => {

                        usb_write(&mut serial, "\r\n");

                        if let Some(mut v) = parse_freq(&cmd) {

                            if v > 200000 { v = 200000; }
                            if v < 100 { v = 100; }

                            DDS_FREQ.store(v, Ordering::Relaxed);

                            let mut buf = String::<32>::new();

                            buf.push_str("Freq: ").ok();
                            format_freq(v, &mut buf);
                            buf.push_str("\r\n> ").ok();

                            usb_write(&mut serial, &buf);

                        } else {

                            usb_write(&mut serial, "Invalid\r\n> ");
                        }

                        cmd.clear();
                    }

                    8 | 127 => {

                        if !cmd.is_empty() {
                            cmd.pop();
                            usb_write(&mut serial, "\x08 \x08");
                        }
                    }

                    _ => {

                        if cmd.push(b as char).is_ok() {
                            let _ = serial.write(&[b]);
                        }
                    }
                }
            }
        }
    }
}

fn usb_write(serial: &mut SerialPort<'_, hal::usb::UsbBus>, s: &str) {
    let mut bytes = s.as_bytes();
    while !bytes.is_empty() {
        match serial.write(bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(_) => {}
        }
    }
}

fn parse_freq(s: &str) -> Option<u32> {

    let s = s.trim();

    if s.is_empty() {
        return Some(DDS_FREQ.load(Ordering::Relaxed));
    }

    let mut mult = 1;

    let mut body = s;

    if let Some(pos) = s.find(['k','K']) {
        mult = 1000;
        body = &s[..pos];
    }

    if let Some(dot) = body.find('.') {

        let (left, right) = body.split_at(dot);
        let right = &right[1..];

        let int_part: u32 = if left.is_empty() { 0 } else { left.parse().ok()? };
        let frac_part: u32 = right.parse().ok()?;

        let scale = 10u32.pow(right.len() as u32);

        let value =
            int_part * mult +
            (frac_part * mult) / scale;

        return Some(value);

    } else {

        let mut v: u32 = body.parse().ok()?;

        if mult == 1000 {

            if let Some(pos) = s.find(['k','K']) {

                let tail = &s[pos+1..];

                if !tail.is_empty() {

                    let frac: u32 = tail.parse().ok()?;
                    let scale = 10u32.pow(tail.len() as u32);

                    v = v * 1000 + (frac * 1000) / scale;
                    return Some(v);
                }
            }

            return Some(v * 1000);
        }

        return Some(v);
    }
}

fn format_freq(freq: u32, out: &mut String<32>) {

    if freq < 1000 {

        let _ = core::fmt::write(out, format_args!("{}", freq));

    } else {

        let k = freq / 1000;
        let r = freq % 1000;

        if r == 0 {

            let _ = core::fmt::write(out, format_args!("{}k", k));

        } else if r % 100 == 0 {

            let _ = core::fmt::write(out, format_args!("{}.{}k", k, r / 100));

        } else if r % 10 == 0 {

            let _ = core::fmt::write(out, format_args!("{}.{:02}k", k, r / 10));

        } else {

            let _ = core::fmt::write(out, format_args!("{}.{:03}k", k, r));

        }
    }
}

fn core1_dds_pio_dma_task(
    sm0: hal::pio::UninitStateMachine<(hal::pac::PIO0, hal::pio::SM0)>,
    installed: InstalledProgram<hal::pac::PIO0>,
    pio_pins: (
        Pin<bank0::Gpio0, FunctionPio0, PullDown>,
        Pin<bank0::Gpio1, FunctionPio0, PullDown>,
        Pin<bank0::Gpio2, FunctionPio0, PullDown>,
        Pin<bank0::Gpio3, FunctionPio0, PullDown>,
        Pin<bank0::Gpio4, FunctionPio0, PullDown>,
        Pin<bank0::Gpio5, FunctionPio0, PullDown>,
        Pin<bank0::Gpio6, FunctionPio0, PullDown>,
        Pin<bank0::Gpio7, FunctionPio0, PullDown>,
        Pin<bank0::Gpio8, FunctionPio0, PullDown>,
        Pin<bank0::Gpio9, FunctionPio0, PullDown>,
        Pin<bank0::Gpio10, FunctionPio0, PullDown>,
        Pin<bank0::Gpio11, FunctionPio0, PullDown>,
        Pin<bank0::Gpio12, FunctionPio0, PullDown>,
        Pin<bank0::Gpio13, FunctionPio0, PullDown>,
        Pin<bank0::Gpio14, FunctionPio0, PullDown>,
        Pin<bank0::Gpio15, FunctionPio0, PullDown>,
    ),
    sys_hz: u32,
    mut interp0: hal::sio::Interp0,
    ch_a: hal::dma::Channel<hal::dma::CH2>,
    ch_b: hal::dma::Channel<hal::dma::CH3>,
) -> ! {

    let d0 = pio_pins.0;

    let int = (sys_hz / PROGRAM_STEP_HZ) as u16;
    let rem = sys_hz % PROGRAM_STEP_HZ;
    let frac = ((rem * 256) / PROGRAM_STEP_HZ) as u8;

    let (mut sm, _rx, tx) = hal::pio::PIOBuilder::from_installed_program(installed)
        .out_pins(d0.id().num, 16)
        .clock_divisor_fixed_point(int, frac)
        .build(sm0);

    sm.set_pindirs((0..16u8).map(|pin| (pin, hal::pio::PinDir::Output)));
    sm.start();

    let mut lane0 = interp0.get_lane0();

    let ctrl = LaneCtrl {
        shift: (32 - LUT_BITS) as u8,
        mask_lsb: 0,
        mask_msb: (LUT_BITS as u8) - 1,
        ..LaneCtrl::new()
    };

    lane0.set_ctrl(ctrl.encode());
    lane0.set_base(0);
    lane0.set_accum(0);

    let b0 = unsafe { dds_buf_mut(0) };
    let b1 = unsafe { dds_buf_mut(1) };

    let mut freq = DDS_FREQ.load(Ordering::Relaxed);
    let mut step = (((freq as u64) << 32) / OUTPUT_HZ as u64) as u32;

    fill_dds_block_interp(&mut lane0, step, b0);
    fill_dds_block_interp(&mut lane0, step, b1);

    let mut tx_transfer =
        double_buffer::Config::new((ch_a, ch_b), b0, tx)
            .start()
            .read_next(b1);

    loop {

        let new_freq = DDS_FREQ.load(Ordering::Relaxed);

        if new_freq != freq {

            freq = new_freq;
            step = (((freq as u64) << 32) / OUTPUT_HZ as u64) as u32;

        }

        let (done_buf, next) = tx_transfer.wait();

        fill_dds_block_interp(&mut lane0, step, done_buf);

        tx_transfer = next.read_next(done_buf);
    }
}

#[inline(always)]
fn fill_dds_block_interp(
    lane0: &mut hal::sio::Interp0Lane0,
    step: u32,
    out: &mut [u32; DDS_BUF_SAMPLES],
) {
    for w in out.iter_mut() {

        lane0.add_accum(step);

        let idx = (lane0.peek() as usize) & (LUT_LEN - 1);

        *w = SINE_LUT[idx] as u32;
    }
}