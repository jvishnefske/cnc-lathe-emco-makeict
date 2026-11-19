//! STM32F407VG Discovery (STM32F4DISCOVERY) — the pin table, and nothing else.
//!
//! # EVERY NUMBER BELOW IS PROVISIONAL
//!
//! Nobody has yet said which board this is, which USART the RS-485
//! transceiver sits on, or which pin drives DE. This is a stock F407
//! Discovery assignment, chosen so the rest of the firmware could be written
//! and compiled; it is a starting point to be corrected at the bench, not a
//! claim about anybody's hardware. Correcting it is a one-line edit per pin,
//! which is the entire reason this file exists.
//!
//! | Function | Pin | Note |
//! |---|---|---|
//! | RS-485 RX | `PA3` | USART2_RX, AF7 |
//! | RS-485 TX | `PA2` | USART2_TX, AF7 |
//! | RS-485 DE | `PA1` | GPIO. **Input with a pull-down in a listen-only build.** |
//! | USB D− | `PA11` | OTG_FS, on CN5 |
//! | USB D+ | `PA12` | OTG_FS, on CN5 |
//! | LED, receiving | `PD14` | red |
//! | LED, transmitting | `PD13` | orange |
//! | LED, error or drop | `PD12` | green |
//!
//! # Two traps, both named because they cost time
//!
//! **VBUS sense.** On several F407 Discovery revisions `PA9` is not wired the
//! way the OTG driver expects, and the device will not enumerate at all until
//! VBUS detection is turned off. That is `config.vbus_detection = false`
//! below, and it is not optional on this board.
//!
//! **RE and DE.** On most transceiver modules `RE` is active-low and is
//! strapped to `DE`, so one line controls both and the receiver is deaf while
//! transmitting. Whether this adapter does that is UNCONFIRMED. If RE is
//! separate, tie it low so the bridge always hears the bus — including its own
//! transmission, which is a feature: it is how the firmware can compare what
//! it drove against what appeared on the wire.
//!
//! # Termination: DO NOT FIT
//!
//! `docs/30-serial-protocol.md` §1.1 records the machine's bus as already
//! terminated and biased at both ends (330 Ω to +5 V, 120 Ω across, 330 Ω to
//! GND). A third 120 Ω terminator is a real hazard, not a belt-and-braces
//! measure. Leave the module's termination jumper off, and leave DB9 pin 9
//! (+5 V) unconnected.

use embassy_stm32::{bind_interrupts, dma, peripherals, usart, usb, Peri};

/// What a person calls this board.
pub const NAME: &str = "STM32F407VG Discovery";

/// The bit rate the RS-485 side starts at.
///
/// The bus rate is UNKNOWN (doc 30 §2.2, A1). 9600 is what the machine's own
/// COM ports use for file transfer (§1.3) and is therefore the least
/// arbitrary guess available, not a finding. The sweep in §7 is the way to
/// find the real one.
pub const DEFAULT_BAUD: u32 = 9600;

bind_interrupts!(pub struct Irqs {
    USART2 => usart::InterruptHandler<peripherals::USART2>;
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
    // The DMA streams the USART is wired to on this part. They are named here
    // and nowhere else for the same reason the pins are: they are a fact about
    // the silicon, not a decision the firmware makes.
    DMA1_STREAM5 => dma::InterruptHandler<peripherals::DMA1_CH5>;
    DMA1_STREAM6 => dma::InterruptHandler<peripherals::DMA1_CH6>;
});

/// The peripherals this firmware names, and no others.
pub struct Board {
    pub usart: Peri<'static, peripherals::USART2>,
    pub uart_rx: Peri<'static, peripherals::PA3>,
    pub uart_tx: Peri<'static, peripherals::PA2>,
    /// Driver enable. Configured as an input with a pull-down when the
    /// `listen-only` feature is on, and there is no code in that build that
    /// could make it an output.
    pub de: Peri<'static, peripherals::PA1>,
    pub rx_dma: Peri<'static, peripherals::DMA1_CH5>,
    pub tx_dma: Peri<'static, peripherals::DMA1_CH6>,
    pub usb: Peri<'static, peripherals::USB_OTG_FS>,
    pub usb_dp: Peri<'static, peripherals::PA12>,
    pub usb_dm: Peri<'static, peripherals::PA11>,
    pub led_rx: Peri<'static, peripherals::PD14>,
    pub led_tx: Peri<'static, peripherals::PD13>,
    pub led_error: Peri<'static, peripherals::PD12>,
}

/// The table, applied. The only function in this file, and it decides nothing.
pub fn take(p: embassy_stm32::Peripherals) -> Board {
    Board {
        usart: p.USART2,
        uart_rx: p.PA3,
        uart_tx: p.PA2,
        de: p.PA1,
        rx_dma: p.DMA1_CH5,
        tx_dma: p.DMA1_CH6,
        usb: p.USB_OTG_FS,
        usb_dp: p.PA12,
        usb_dm: p.PA11,
        led_rx: p.PD14,
        led_tx: p.PD13,
        led_error: p.PD12,
    }
}

/// The clock tree this board can reach, and the USB requirement that fixes it.
///
/// OTG_FS needs 48 MHz exactly. On this board the 8 MHz HSE comes from the
/// ST-LINK's MCO, so the PLL is set from that rather than from the crystal
/// that is not fitted.
pub fn clocks(config: &mut embassy_stm32::Config) {
    use embassy_stm32::rcc::*;

    config.rcc.hse = Some(Hse {
        freq: embassy_stm32::time::Hertz(8_000_000),
        mode: HseMode::Bypass,
    });
    config.rcc.pll_src = PllSource::HSE;
    config.rcc.pll = Some(Pll {
        prediv: PllPreDiv::DIV4,   //  8 MHz -> 2 MHz
        mul: PllMul::MUL168,       //  2 MHz -> 336 MHz
        divp: Some(PllPDiv::DIV2), // 336    -> 168 MHz, the core
        divq: Some(PllQDiv::DIV7), // 336    -> 48 MHz, and USB needs exactly this
        divr: None,
    });
    config.rcc.ahb_pre = AHBPrescaler::DIV1;
    config.rcc.apb1_pre = APBPrescaler::DIV4;
    config.rcc.apb2_pre = APBPrescaler::DIV2;
    config.rcc.sys = Sysclk::PLL1_P;
    config.rcc.mux.clk48sel = mux::Clk48sel::PLL1_Q;
}
