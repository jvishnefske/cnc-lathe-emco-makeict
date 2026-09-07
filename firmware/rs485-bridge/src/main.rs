//! `emco-rs485-bridge` — a safe, always-attached, timestamped RS-485 capture
//! device for the EMCO PC Turn 50.
//!
//! # Why this exists
//!
//! The machine's PC link is a 3-wire half-duplex RS-485 multidrop bus and its
//! protocol is unknown (`docs/30-serial-protocol.md`). Discovery needs a
//! capture, and a USB-RS485 dongle plus `pyserial` cannot produce one worth
//! having:
//!
//!   * Host timestamps are taken at `read()`, after USB's 1 ms frames, after
//!     `cdc_acm` buffering and after scheduler latency. At 9600 baud a
//!     character is 1042 µs, so host timing destroys exactly the inter-byte
//!     and inter-frame signature that identifies a polled multidrop protocol.
//!   * On 3-wire half-duplex, once transmit is ever armed, your own echo and a
//!     reply are the same bytes on the same conductors.
//!   * `pyserial` cannot see a framing or parity error at all, and doc 30
//!     records that the wire MAY NOT BE ASYNC-UART AT ALL — so an error at a
//!     known timestamp is a POSITIVE finding.
//!
//! It is NOT a replacement for a logic analyser across `DATA±`, which doc 30
//! rung 2 rightly names as the better first instrument. It is the instrument
//! you can leave attached through a power cycle.
//!
//! # Two USB ports
//!
//! This enumerates as a composite device with two CDC-ACM interfaces:
//!
//!   * **port 1, raw passthrough.** Every byte, nothing added, best-effort.
//!     `tools/emco-listen.py` reads this UNCHANGED — that tool is the
//!     safety-reviewed one with the RS-232 warnings and it is not touched.
//!   * **port 2, framed records.** COBS-delimited, with a hardware microsecond
//!     timestamp, a direction tag, and the UART's own error flags.
//!     `tools/emco-capture.py` reads this. It is the evidence port, and it is
//!     the one with the accounting: nothing it drops is dropped silently.
//!
//! # Listen-only is a property, not a mode
//!
//! With the default `listen-only` feature, the transmit path is `#[cfg]`-ed
//! out of the binary and DE is an input with a pull-down. A bug cannot assert
//! a driver whose code does not exist. On the hardware, DE is pulled to GND on
//! the adapter and reaches the MCU only through a link not fitted by default.
//! And every `STATUS` record names the build variant, so a transcript proves
//! which firmware produced it.
//!
//! # What has NOT been verified
//!
//! Nobody has run this on a board. Stated plainly because the rest of this
//! project distinguishes carefully between what is known and what is assumed:
//!
//!   * **USB enumeration.** Untested. See the VBUS trap in
//!     `src/board/f407_discovery.rs`.
//!   * **Timestamp accuracy.** The stamp is taken in the task woken by the
//!     byte, not in the ISR, so it carries executor and DMA-completion
//!     latency. The measurement that matters is the GAP between two bytes,
//!     and a constant latency cancels out of a difference — only jitter
//!     survives. How much jitter is an empirical question, and the bench
//!     experiment in the README answers it: loop RX to a USB-RS485 dongle
//!     sending a known pattern at a known bit rate and compare the reported
//!     inter-byte deltas against the theoretical character time. No lathe
//!     required.
//!   * **DE turnaround.** Untested at any bit rate.
//!   * **The pin table.** Provisional; see `src/board/`.
//!
//! Nothing here is a safety function. `docs/30-serial-protocol.md` §5.

#![no_std]
#![no_main]

mod board;

use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};
use embassy_stm32::gpio::{Level, Output, Speed};
#[cfg(feature = "listen-only")]
use embassy_stm32::gpio::{Input, Pull};
use embassy_stm32::mode::Async;
use embassy_stm32::usart::{self, UartRx};
#[cfg(not(feature = "listen-only"))]
use embassy_stm32::usart::{Uart, UartTx};
use embassy_stm32::usb::{self, Driver};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, Sender, State};
#[cfg(not(feature = "listen-only"))]
use embassy_usb::class::cdc_acm::Receiver;
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config as UsbConfig};
use static_cell::StaticCell;

use emco_bridge_core::record::flag;
use emco_bridge_core::{event, Bridge, Config as BridgeConfig, PROTOCOL_VERSION};

use panic_halt as _;

/// What the `STATUS` record's description says this build is.
const VARIANT: &str = if cfg!(feature = "listen-only") { "listen-only" } else { "tx-armed" };

/// The outbound record queue, in bytes.
///
/// USB drains in ~1 ms bursts; a 115200-baud bus produces ~11.5 kB/s, so this
/// covers roughly half a second of a fully busy bus with the host asleep.
/// When it is not enough, records are dropped and COUNTED, and the next one to
/// leave carries `DROPPED`.
const QUEUE: usize = 8192;

/// How often the counters and the build variant are re-sent.
const STATUS_EVERY: Duration = Duration::from_secs(5);

/// How long the record task waits for a byte before closing an open record.
const IDLE_POLL: Duration = Duration::from_millis(5);

/// One captured byte on its way from the UART task to the record task.
type Captured = (u32, u8, u8);

static CAPTURED: Channel<CriticalSectionRawMutex, Captured, 512> = Channel::new();
/// Best-effort, by design. The raw port is for eyeballing with an existing
/// tool; the record port is the evidence, and it is the one that accounts for
/// everything it loses.
static RAW: Channel<CriticalSectionRawMutex, u8, 256> = Channel::new();
/// Bytes this device drove onto the bus, on their way to being recorded as
/// ours. Empty and never written to in a listen-only build, because the code
/// that would write to it is not compiled.
static TRANSMITTED: Channel<CriticalSectionRawMutex, (u32, u8), 128> = Channel::new();

/// Free-running microseconds, truncated to the 32 bits the record format
/// carries. It wraps every ~71.6 minutes and the HOST un-wraps it — see
/// `tools/emco-capture.py`, which is the side with room for a 64-bit counter
/// and a session length.
fn micros() -> u32 {
    Instant::now().as_micros() as u32
}

// --------------------------------------------------------------------- tasks

/// One byte at a time, with the time it arrived and what the UART said of it.
///
/// A byte at a time rather than DMA with IDLE-line segmentation, and the
/// reason is the whole point of the device: DMA gives you a burst and one
/// timestamp for it, and the inter-byte gaps inside that burst would then have
/// to be RECONSTRUCTED from an assumed bit rate. The bit rate is what we are
/// trying to find out (doc 30 §2.2, A1), so reconstructing it would be
/// fabricating the measurement. Per-byte error attribution goes the same way.
///
/// The cost is one wake per byte: 960/s at 9600 baud, 11 520/s at 115 200. On
/// a 168 MHz part with nothing else to do, that is affordable, and the
/// segmentation that DMA would have done in hardware is done in
/// `emco-bridge-core` instead — where it is `cargo test`-able on a laptop.
#[embassy_executor::task]
async fn capture(mut rx: UartRx<'static, Async>) {
    let mut byte = [0u8; 1];
    loop {
        let flags = match rx.read(&mut byte).await {
            Ok(()) => 0,
            Err(usart::Error::Framing) => flag::FRAMING,
            Err(usart::Error::Parity) => flag::PARITY,
            Err(usart::Error::Noise) => flag::NOISE,
            Err(usart::Error::Overrun) => flag::OVERRUN,
            // BufferTooLong cannot happen for one byte, and a future variant
            // is still a byte that arrived: report it as an unspecified
            // condition rather than dropping it.
            Err(_) => flag::BREAK,
        };
        let at = micros();
        // Full queue: drop, rather than block the UART and cause an overrun
        // that would be attributed to the bus. The record task's own
        // accounting is what makes the gap visible.
        let _ = CAPTURED.try_send((at, byte[0], flags));
        let _ = RAW.try_send(byte[0]);
    }
}

/// Segments, frames and sends. Owns the `Bridge`, so nothing is shared.
#[embassy_executor::task]
async fn records(
    mut port: CdcAcmClass<'static, Driver<'static, embassy_stm32::peripherals::USB_OTG_FS>>,
    mut led_rx: Output<'static>,
    mut led_error: Output<'static>,
    baud: u32,
) {
    let mut bridge: Bridge<QUEUE> = Bridge::new(BridgeConfig::at_baud(baud));
    let mut out = [0u8; 64];
    let mut text = [0u8; 96];
    let described = describe(&mut text, baud);
    let described = core::str::from_utf8(&text[..described]).unwrap_or("emco-rs485-bridge");
    let variant = if cfg!(feature = "listen-only") {
        flag::LISTEN_ONLY
    } else {
        flag::TX_ARMED
    };

    loop {
        port.wait_connection().await;
        // A transcript opens by naming the format it is in and the build that
        // produced it, so a capture file is self-evidencing even when it turns
        // up on its own months later.
        bridge.on_event(micros(), event::BOOT, PROTOCOL_VERSION);
        bridge.on_status(micros(), variant, described);
        let mut next_status = Instant::now() + STATUS_EVERY;

        loop {
            // A received byte, a byte we sent, or enough time passed that an
            // open record should be closed. The third is not a nicety: on a
            // bus that may be silent for hours, waiting for the next byte to
            // flush the last one means waiting for ever.
            match select3(
                CAPTURED.receive(),
                TRANSMITTED.receive(),
                Timer::after(IDLE_POLL),
            )
            .await
            {
                Either3::First((at, byte, flags)) => {
                    bridge.on_rx(at, byte, flags);
                    led_rx.toggle();
                    if flags != 0 {
                        led_error.set_high();
                    }
                }
                Either3::Second((at, byte)) => bridge.on_tx(at, byte),
                Either3::Third(()) => bridge.on_idle(micros()),
            }

            // The counters, periodically. Also what keeps the host's clock
            // un-wrapping honest: two records more than 71.6 minutes apart
            // would be ambiguous, and a periodic status makes that gap
            // impossible in practice.
            if Instant::now() >= next_status {
                bridge.on_status(micros(), variant, described);
                next_status = Instant::now() + STATUS_EVERY;
            }

            let n = bridge.read(&mut out);
            if n > 0 && write_all(&mut port, &out[..n]).await.is_err() {
                break;                       // unplugged; wait for the next host
            }
        }
    }
}

/// Host bytes onto the bus, with the driver enabled around them.
///
/// Not compiled at all in a listen-only build. That is the point: the
/// interlock is the absence of this function from the binary, not a flag
/// somewhere that a bug could get wrong.
#[cfg(not(feature = "listen-only"))]
#[embassy_executor::task]
async fn transmit(
    mut port: Receiver<'static, Driver<'static, embassy_stm32::peripherals::USB_OTG_FS>>,
    mut uart: UartTx<'static, Async>,
    mut de: Output<'static>,
    baud: u32,
) {
    // One character at 8N1: ten bits. Computed from the bit rate the model
    // already carries rather than written down, so changing the rate cannot
    // leave a hold time behind that was right for the old one.
    let character = Duration::from_micros((10 * 1_000_000 / baud as u64).max(1));
    let mut packet = [0u8; 64];

    loop {
        port.wait_connection().await;
        loop {
            let n = match port.read_packet(&mut packet).await {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                continue;
            }
            de.set_high();
            // The transceiver needs the driver settled before the first bit.
            Timer::after(character).await;
            let at = micros();
            if uart.write(&packet[..n]).await.is_err() {
                de.set_low();
                continue;
            }
            let _ = uart.blocking_flush();

            // RELEASING DE AFTER `flush()` IS NOT SUFFICIENT, and this is the
            // detail that makes a bus "mostly work". `embedded_io::Write::flush`
            // promises the data has left the driver's buffers; it does not
            // promise the shift register has finished clocking the last
            // character onto the wire. On this part the TC flag does mean
            // that, but relying on a guarantee the TRAIT does not make is how
            // the last byte of a frame goes missing on the next part somebody
            // ports this to. One character time costs nothing and closes it.
            Timer::after(character).await;
            de.set_low();

            // Recorded as ours, byte by byte, so the transcript can tell our
            // echo from a reply. On a 3-wire half-duplex bus they are the same
            // bytes on the same conductors and nothing downstream could.
            let mut when = at;
            for byte in &packet[..n] {
                let _ = TRANSMITTED.try_send((when, *byte));
                when = when.wrapping_add(character.as_micros() as u32);
            }
        }
    }
}

/// The passthrough port: every byte, nothing added.
#[embassy_executor::task]
async fn passthrough(
    mut port: Sender<'static, Driver<'static, embassy_stm32::peripherals::USB_OTG_FS>>,
) {
    let mut out = [0u8; 64];
    loop {
        port.wait_connection().await;
        loop {
            let mut n = 0;
            out[n] = RAW.receive().await;
            n += 1;
            // Whatever else is already waiting goes in the same packet: one
            // USB transaction per byte would cap the bridge at about 1000
            // bytes a second, which is slower than the slowest bus rate we
            // might meet.
            while n < out.len() {
                match RAW.try_receive() {
                    Ok(byte) => {
                        out[n] = byte;
                        n += 1;
                    }
                    Err(_) => break,
                }
            }
            if write_all(&mut port, &out[..n]).await.is_err() {
                break;
            }
        }
    }
}

#[embassy_executor::task]
async fn run_usb(
    mut device: embassy_usb::UsbDevice<'static, Driver<'static, embassy_stm32::peripherals::USB_OTG_FS>>,
) -> ! {
    device.run().await
}

/// The one thing both ends of a split CDC class have in common.
///
/// `CdcAcmClass` and its `Sender` half both write packets and both know their
/// packet size, and the record port keeps the whole class while the raw port
/// is split. Two copies of `write_all` for that difference would be two places
/// for the same short-packet rule to be wrong.
trait Packets {
    fn size(&self) -> usize;
    async fn packet(&mut self, bytes: &[u8]) -> Result<(), EndpointError>;
}

impl<'d, D: embassy_usb::driver::Driver<'d>> Packets for CdcAcmClass<'d, D> {
    fn size(&self) -> usize {
        self.max_packet_size() as usize
    }
    async fn packet(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
        self.write_packet(bytes).await
    }
}

impl<'d, D: embassy_usb::driver::Driver<'d>> Packets for Sender<'d, D> {
    fn size(&self) -> usize {
        self.max_packet_size() as usize
    }
    async fn packet(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
        self.write_packet(bytes).await
    }
}

async fn write_all<P: Packets>(port: &mut P, mut bytes: &[u8]) -> Result<(), EndpointError> {
    let size = port.size();
    while !bytes.is_empty() {
        let take = bytes.len().min(size);
        port.packet(&bytes[..take]).await?;
        bytes = &bytes[take..];
        // A transfer whose last packet is exactly the endpoint size needs a
        // zero-length packet to end it, or the host waits for more that never
        // comes. Records are the wrong place to discover that.
        if bytes.is_empty() && take == size {
            port.packet(&[]).await?;
        }
    }
    Ok(())
}

/// The `STATUS` description, built without a formatter.
///
/// `core::fmt` would pull in a formatting machine for one line of text on a
/// part where flash is the budget. This is the same line, written out.
fn describe(out: &mut [u8; 96], baud: u32) -> usize {
    fn put(out: &mut [u8; 96], at: &mut usize, text: &str) {
        for byte in text.as_bytes() {
            if *at < out.len() {
                out[*at] = *byte;
                *at += 1;
            }
        }
    }

    let mut n = 0;
    put(out, &mut n, "emco-rs485-bridge ");
    put(out, &mut n, env!("CARGO_PKG_VERSION"));
    put(out, &mut n, " ");
    put(out, &mut n, VARIANT);
    put(out, &mut n, " ");

    let mut digits = [0u8; 10];
    let mut count = 0;
    let mut value = baud;
    if value == 0 {
        digits[0] = b'0';
        count = 1;
    }
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }
    while count > 0 && n < out.len() {
        count -= 1;
        out[n] = digits[count];
        n += 1;
    }

    put(out, &mut n, " 8N1 ");
    put(out, &mut n, board::NAME);
    n
}

// ---------------------------------------------------------------------- main

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_stm32::Config::default();
    board::clocks(&mut config);
    let peripherals = embassy_stm32::init(config);
    let hardware = board::take(peripherals);

    // ---- the RS-485 side ------------------------------------------------
    //
    // DE first, and before anything else can drive the bus. In a listen-only
    // build it is an INPUT with a pull-down: there is no code in this binary
    // that could make it an output, which is a stronger claim than any flag.
    #[cfg(feature = "listen-only")]
    let _de = Input::new(hardware.de, Pull::Down);
    #[cfg(not(feature = "listen-only"))]
    let de = Output::new(hardware.de, Level::Low, Speed::VeryHigh);

    let mut uart = usart::Config::default();
    uart.baudrate = board::DEFAULT_BAUD;
    // 8N1 to start with. The bus framing is unknown and may not be async at
    // all (doc 30 §2.2, A1); the sweep in §7 is how the real one is found, and
    // a framing error reported at a known timestamp is what makes the sweep
    // informative rather than a guess.
    uart.data_bits = usart::DataBits::DataBits8;
    uart.parity = usart::Parity::ParityNone;
    uart.stop_bits = usart::StopBits::STOP1;
    // Errors must reach the record, not be swallowed by the driver: they are
    // the evidence this device exists to collect.
    uart.detect_previous_overrun = true;

    // In a listen-only build the USART is constructed RECEIVE-ONLY, and the TX
    // pin is dropped without ever being configured: it stays the
    // high-impedance input it is after reset. That is one more layer of the
    // same interlock — the transceiver's DI is not driven by a pin this
    // firmware has claimed, let alone by data.
    #[cfg(feature = "listen-only")]
    let rx = {
        drop((hardware.uart_tx, hardware.tx_dma));
        UartRx::new(
            hardware.usart,
            hardware.uart_rx,
            hardware.rx_dma,
            board::Irqs,
            uart,
        )
        .expect("a UART configuration this board's clocks cannot reach")
    };

    // A transmitting build takes the whole USART and splits it. Two separate
    // constructions are not possible, and should not be: one peripheral, one
    // owner, halved by the driver that knows how.
    #[cfg(not(feature = "listen-only"))]
    let (uart_tx, rx) = Uart::new(
        hardware.usart,
        hardware.uart_rx,
        hardware.uart_tx,
        hardware.tx_dma,
        hardware.rx_dma,
        board::Irqs,
        uart,
    )
    .expect("a UART configuration this board's clocks cannot reach")
    .split();

    // ---- USB: one device, two CDC-ACM functions -------------------------
    static EP_OUT: StaticCell<[u8; 256]> = StaticCell::new();
    let mut usb = usb::Config::default();
    // NOT OPTIONAL ON THIS BOARD. On several F407 Discovery revisions PA9 is
    // not wired the way the driver expects, and the device does not enumerate
    // at all until this is off. See `src/board/f407_discovery.rs`.
    usb.vbus_detection = false;
    let driver = Driver::new_fs(
        hardware.usb,
        board::Irqs,
        hardware.usb_dp,
        hardware.usb_dm,
        EP_OUT.init([0; 256]),
        usb,
    );

    let mut config = UsbConfig::new(0x1209, 0x0001);
    config.manufacturer = Some("makeict");
    config.product = Some("EMCO RS-485 capture bridge");
    config.serial_number = Some(VARIANT);
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    // Two functions on one device need Interface Association Descriptors, and
    // the USB-IF assigns fixed class codes to a device that carries them.
    config.composite_with_iads = true;
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;

    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 32]> = StaticCell::new();
    static MSOS_DESCRIPTOR: StaticCell<[u8; 32]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 128]> = StaticCell::new();
    static RAW_STATE: StaticCell<State> = StaticCell::new();
    static RECORD_STATE: StaticCell<State> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 32]),
        MSOS_DESCRIPTOR.init([0; 32]),
        CONTROL_BUF.init([0; 128]),
    );

    // Order matters to the host: the FIRST interface is the lower-numbered
    // /dev/ttyACM*, and `tools/emco-listen.py` expects the raw one. Getting
    // this the wrong way round would feed a records stream to a tool that
    // prints it as bus bytes, which looks like a machine talking gibberish.
    let raw_port = CdcAcmClass::new(&mut builder, RAW_STATE.init(State::new()), 64);
    let record_port = CdcAcmClass::new(&mut builder, RECORD_STATE.init(State::new()), 64);
    let device = builder.build();

    // The raw port is split so its two halves can be owned by two tasks: what
    // the bus said goes out, and — only in a transmitting build — what the
    // host wants to say comes in.
    let (raw_out, raw_in) = raw_port.split();
    #[cfg(feature = "listen-only")]
    drop(raw_in);

    // ---- lights ---------------------------------------------------------
    let led_rx = Output::new(hardware.led_rx, Level::Low, Speed::Low);
    let led_error = Output::new(hardware.led_error, Level::Low, Speed::Low);
    let _led_tx = Output::new(hardware.led_tx, Level::Low, Speed::Low);

    spawner.spawn(run_usb(device).expect("the task pool was already full"));
    spawner.spawn(capture(rx).expect("the task pool was already full"));
    spawner.spawn(passthrough(raw_out).expect("the task pool was already full"));
    spawner.spawn(records(record_port, led_rx, led_error, board::DEFAULT_BAUD).expect("the task pool was already full"));
    #[cfg(not(feature = "listen-only"))]
    spawner.spawn(
        transmit(raw_in, uart_tx, de, board::DEFAULT_BAUD)
            .expect("the task pool was already full"),
    );
}
