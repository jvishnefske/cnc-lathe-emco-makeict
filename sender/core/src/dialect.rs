//! Wire-protocol dialects.
//!
//! **The real machine's protocol is UNKNOWN.** The PC link is RS-485
//! (control-box connector `X300`: pin 2 GND, pin 3 DATA-, pin 8 DATA+),
//! driven on the original DOS PC by a proprietary memory-mapped ISA card
//! (`Y4A013000`, 4 KB dual-port RAM at CC000-CFFFF) - **not a UART**. There
//! is therefore no known byte-stream framing at all; see
//! `docs/_machine-facts.md` and `docs/30-serial-protocol.md`. Every framing
//! assumption lives behind the [`Dialect`] trait so that swapping it out
//! never touches the UI, the backplot, or `Session`'s state machine.

use crate::gcode::Block;

/// Something the machine's byte stream told us.
#[derive(Clone, Debug, PartialEq)]
pub enum DialectEvent {
    /// Block accepted / completed.
    Ack,
    /// Block rejected; `message` is whatever error text was captured.
    Nak { message: String },
    /// Asynchronous alarm/error report from the machine.
    Alarm { code: String, message: String },
    /// Software flow control: the machine asked us to pause transmission.
    XOff,
    /// Software flow control: the machine is ready for more.
    XOn,
    /// A complete text line that was none of the above (status chatter, echo).
    Text(String),
}

/// The protocol boundary from `sender/INTERFACE.md`.
///
/// The three contract methods are here verbatim. The extra methods
/// (`note_block_sent`, `set_cts`, `control_bytes`, `reset`) are additions the
/// core needs to drive flow control and realtime actions; they are part of the
/// trait so a corrected real-machine implementation can override them.
pub trait Dialect {
    /// Frame one block for the wire.
    fn encode_block(&self, block: &Block) -> Vec<u8>;

    /// Consume received bytes, returning protocol events. Stateful: partial
    /// lines are buffered across calls.
    fn feed_rx(&mut self, bytes: &[u8]) -> Vec<DialectEvent>;

    /// May the session send another block right now? Accounts for outstanding
    /// unacknowledged blocks, XON/XOFF state and hardware CTS.
    fn wants_next_block(&self) -> bool;

    /// Tell the dialect a block was handed to the transport (so it can count
    /// outstanding blocks).
    fn note_block_sent(&mut self);

    /// Hardware flow control: the UI reports the CTS line state here when the
    /// transport exposes it. Defaults to asserted.
    fn set_cts(&mut self, asserted: bool);

    /// Bytes for a realtime control action, or empty if the dialect has none.
    fn control_bytes(&self, action: ControlAction) -> Vec<u8>;

    /// Forget buffered rx and outstanding-block state (used on abort/reconnect).
    fn reset(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAction {
    /// Best-effort stop. Advisory only — never a safety function.
    Abort,
    FeedHold,
    Resume,
    /// Ask the machine to home / reference its axes.
    Home,
}

// ---------------------------------------------------------------------------
// PlainGcode
// ---------------------------------------------------------------------------

/// Line-oriented plain G-code dialect.
///
/// **This is a development and training fiction, not the machine protocol.**
/// The real PC Turn 50 link is RS-485 driven by a memory-mapped ISA card with
/// no known byte-stream framing, so nothing below describes the real machine.
/// `PlainGcode` exists so the [`crate::machine::VirtualMachine`] (which
/// speaks it natively), the tests and the UI are all exercisable today, and
/// so retrofit controllers with a text protocol have something to talk to.
///
/// Framing (of the fiction):
/// - Blocks are sent as ASCII text terminated by CR (`\r`).
/// - The machine acknowledges each block with a line starting with `ok`
///   (case-insensitive) or a bare ACK byte (0x06).
/// - Rejections are a line starting with `error` or a bare NAK byte (0x15);
///   trailing text on the line is captured as the error message.
/// - Asynchronous alarms are lines starting with `alarm:`; the first word
///   after the colon is the code, the rest the message.
/// - XON (0x11) / XOFF (0x13) gate transmission in addition to hardware CTS.
/// - At most one block is outstanding (sent but unacknowledged) at a time.
pub struct PlainGcode {
    outstanding: usize,
    max_outstanding: usize,
    xoff: bool,
    cts: bool,
    line_buf: String,
}

impl PlainGcode {
    pub fn new() -> PlainGcode {
        PlainGcode {
            outstanding: 0,
            max_outstanding: 1,
            xoff: false,
            cts: true,
            line_buf: String::new(),
        }
    }

    pub fn outstanding(&self) -> usize {
        self.outstanding
    }

    fn classify_line(line: &str) -> Option<DialectEvent> {
        let t = line.trim();
        if t.is_empty() {
            return None;
        }
        let lower = t.to_ascii_lowercase();
        if lower == "ok" || lower.starts_with("ok ") || lower.starts_with("ok:") {
            return Some(DialectEvent::Ack);
        }
        if lower.starts_with("error") {
            let msg = t["error".len()..].trim_start_matches([':', ' ']).to_string();
            let msg = if msg.is_empty() { "error".to_string() } else { msg };
            return Some(DialectEvent::Nak { message: msg });
        }
        if let Some(rest) = t.strip_prefix("alarm:").or_else(|| t.strip_prefix("ALARM:")) {
            let rest = rest.trim();
            let mut parts = rest.splitn(2, ' ');
            let code = parts.next().unwrap_or("").to_string();
            let message = parts.next().unwrap_or("").trim().to_string();
            return Some(DialectEvent::Alarm { code, message });
        }
        Some(DialectEvent::Text(t.to_string()))
    }
}

impl Default for PlainGcode {
    fn default() -> Self {
        Self::new()
    }
}

impl Dialect for PlainGcode {
    fn encode_block(&self, block: &Block) -> Vec<u8> {
        // Comments and redundant whitespace were stripped by the lexer; send
        // the cleaned word text, CR terminated (a fiction; see the type docs).
        let mut v = block.clean.as_bytes().to_vec();
        v.push(b'\r');
        v
    }

    fn feed_rx(&mut self, bytes: &[u8]) -> Vec<DialectEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            match b {
                0x11 => {
                    self.xoff = false;
                    events.push(DialectEvent::XOn);
                }
                0x13 => {
                    self.xoff = true;
                    events.push(DialectEvent::XOff);
                }
                0x06 => {
                    self.outstanding = self.outstanding.saturating_sub(1);
                    events.push(DialectEvent::Ack);
                }
                0x15 => {
                    self.outstanding = self.outstanding.saturating_sub(1);
                    events.push(DialectEvent::Nak { message: "NAK".to_string() });
                }
                b'\r' | b'\n' => {
                    let line = std::mem::take(&mut self.line_buf);
                    if let Some(ev) = Self::classify_line(&line) {
                        match &ev {
                            DialectEvent::Ack | DialectEvent::Nak { .. } => {
                                self.outstanding = self.outstanding.saturating_sub(1);
                            }
                            _ => {}
                        }
                        events.push(ev);
                    }
                }
                _ => {
                    if b.is_ascii() && !b.is_ascii_control() {
                        self.line_buf.push(b as char);
                        // Defensive cap so hostile input can't grow the buffer forever.
                        if self.line_buf.len() > 4096 {
                            self.line_buf.clear();
                        }
                    }
                }
            }
        }
        events
    }

    fn wants_next_block(&self) -> bool {
        self.outstanding < self.max_outstanding && !self.xoff && self.cts
    }

    fn note_block_sent(&mut self) {
        self.outstanding += 1;
    }

    fn set_cts(&mut self, asserted: bool) {
        self.cts = asserted;
    }

    fn control_bytes(&self, action: ControlAction) -> Vec<u8> {
        match action {
            // UNVERIFIED: Ctrl-X as a soft-reset/stop is a GRBL-ism used here
            // as a placeholder. Advisory only — never a safety function.
            ControlAction::Abort => vec![0x18],
            // UNVERIFIED: no known realtime feed-hold byte; the session stops
            // queueing blocks instead, which is the only hold we can promise.
            ControlAction::FeedHold => vec![],
            ControlAction::Resume => vec![],
            // UNVERIFIED: sent as a plain G28 reference-return block.
            ControlAction::Home => b"G28\r".to_vec(),
        }
    }

    fn reset(&mut self) {
        self.outstanding = 0;
        self.xoff = false;
        self.line_buf.clear();
    }
}

// ---------------------------------------------------------------------------
// EmcoStub
// ---------------------------------------------------------------------------

/// The real EMCO PC Turn 50 dialect - an explicit stub.
///
/// There is no "AC95" board in this machine (that part number, `Y4B081000`,
/// is community lore for a different EMCO model); the brain is the axis
/// controller / CPU board `Y4A012000` (+ `Y4A015000`), reached over RS-485
/// from a memory-mapped ISA card. **No byte-stream protocol is known**, so
/// every behaviour in here is a placeholder delegating to [`PlainGcode`],
/// with `TODO(protocol)` seams where discovery findings from
/// `docs/30-serial-protocol.md` will land. Correct one seam at a time as
/// facts arrive; never let an assumption in here leak past the `Dialect`
/// trait.
pub struct EmcoStub {
    inner: PlainGcode,
}

impl EmcoStub {
    pub fn new() -> EmcoStub {
        EmcoStub { inner: PlainGcode::new() }
    }
}

impl Default for EmcoStub {
    fn default() -> Self {
        Self::new()
    }
}

impl Dialect for EmcoStub {
    fn encode_block(&self, block: &Block) -> Vec<u8> {
        // TODO(protocol): the real framing is completely unknown (the wire is
        // RS-485 fed by a dual-port-RAM ISA card, not a UART). CR-terminated
        // text below is a placeholder inherited from PlainGcode.
        self.inner.encode_block(block)
    }

    fn feed_rx(&mut self, bytes: &[u8]) -> Vec<DialectEvent> {
        // TODO(protocol): ack/nak vocabulary and alarm format unknown.
        // Currently assumes the PlainGcode fiction documented above.
        self.inner.feed_rx(bytes)
    }

    fn wants_next_block(&self) -> bool {
        // TODO(protocol): flow-control model unknown (window size, pacing).
        // XON/XOFF + CTS below is the PlainGcode fiction.
        self.inner.wants_next_block()
    }

    fn note_block_sent(&mut self) {
        self.inner.note_block_sent();
    }

    fn set_cts(&mut self, asserted: bool) {
        self.inner.set_cts(asserted);
    }

    fn control_bytes(&self, action: ControlAction) -> Vec<u8> {
        // TODO(protocol): no realtime stop/hold bytes are known for the real
        // control. Until then, PlainGcode's placeholders are used.
        self.inner.control_bytes(action)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

/// Construct the dialect selected by a profile.
pub fn make_dialect(kind: crate::profile::DialectKind) -> Box<dyn Dialect> {
    match kind {
        crate::profile::DialectKind::Emco => Box::new(EmcoStub::new()),
        crate::profile::DialectKind::PlainGcode => Box::new(PlainGcode::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::lex_line;

    fn block(s: &str) -> Block {
        let mut d = Vec::new();
        lex_line(s, 0, &mut d).unwrap()
    }

    #[test]
    fn encode_is_cr_terminated_clean_text() {
        let d = PlainGcode::new();
        let b = block("  N10 G01 X12.5 (cut)  Z-4.0 ; note");
        let bytes = d.encode_block(&b);
        assert_eq!(bytes, b"N10 G01 X12.5 Z-4.0\r".to_vec());
    }

    #[test]
    fn framing_round_trip_through_virtual_machine_style_ack() {
        let mut d = PlainGcode::new();
        assert!(d.wants_next_block());
        d.note_block_sent();
        assert!(!d.wants_next_block(), "one block outstanding blocks the next");
        let ev = d.feed_rx(b"ok\r\n");
        assert_eq!(ev, vec![DialectEvent::Ack]);
        assert!(d.wants_next_block());
    }

    #[test]
    fn ack_split_across_rx_chunks() {
        let mut d = PlainGcode::new();
        d.note_block_sent();
        assert!(d.feed_rx(b"o").is_empty());
        assert!(d.feed_rx(b"k").is_empty());
        let ev = d.feed_rx(b"\r");
        assert_eq!(ev, vec![DialectEvent::Ack]);
    }

    #[test]
    fn error_text_is_captured() {
        let mut d = PlainGcode::new();
        d.note_block_sent();
        let ev = d.feed_rx(b"error: soft limit Z\r");
        assert_eq!(ev, vec![DialectEvent::Nak { message: "soft limit Z".to_string() }]);
        assert!(d.wants_next_block(), "nak releases the outstanding slot");
    }

    #[test]
    fn alarm_line_parses_code_and_message() {
        let mut d = PlainGcode::new();
        let ev = d.feed_rx(b"alarm: ESTOP emergency stop pressed\r\n");
        assert_eq!(
            ev,
            vec![DialectEvent::Alarm {
                code: "ESTOP".to_string(),
                message: "emergency stop pressed".to_string()
            }]
        );
    }

    #[test]
    fn xon_xoff_gate_transmission() {
        let mut d = PlainGcode::new();
        assert!(d.wants_next_block());
        let ev = d.feed_rx(&[0x13]);
        assert_eq!(ev, vec![DialectEvent::XOff]);
        assert!(!d.wants_next_block());
        let ev = d.feed_rx(&[0x11]);
        assert_eq!(ev, vec![DialectEvent::XOn]);
        assert!(d.wants_next_block());
    }

    #[test]
    fn cts_gates_transmission() {
        let mut d = PlainGcode::new();
        d.set_cts(false);
        assert!(!d.wants_next_block());
        d.set_cts(true);
        assert!(d.wants_next_block());
    }

    #[test]
    fn raw_ack_nak_bytes() {
        let mut d = PlainGcode::new();
        d.note_block_sent();
        assert_eq!(d.feed_rx(&[0x06]), vec![DialectEvent::Ack]);
        d.note_block_sent();
        assert_eq!(
            d.feed_rx(&[0x15]),
            vec![DialectEvent::Nak { message: "NAK".to_string() }]
        );
    }

    #[test]
    fn unknown_chatter_becomes_text_events() {
        let mut d = PlainGcode::new();
        let ev = d.feed_rx(b"EMCO PC TURN 50 READY\r\n");
        assert_eq!(ev, vec![DialectEvent::Text("EMCO PC TURN 50 READY".to_string())]);
    }

    #[test]
    fn reset_clears_outstanding_and_xoff() {
        let mut d = PlainGcode::new();
        d.note_block_sent();
        d.feed_rx(&[0x13]);
        d.feed_rx(b"partial line");
        d.reset();
        assert!(d.wants_next_block());
        // The partial buffer must not leak into the next line.
        let ev = d.feed_rx(b"ok\r");
        assert_eq!(ev, vec![DialectEvent::Ack]);
    }

    #[test]
    fn emco_stub_delegates_to_plain_for_now() {
        let mut a = EmcoStub::new();
        let b = block("G1 X5 F0.1");
        assert_eq!(a.encode_block(&b), PlainGcode::new().encode_block(&b));
        a.note_block_sent();
        assert!(!a.wants_next_block());
        assert_eq!(a.feed_rx(b"ok\r"), vec![DialectEvent::Ack]);
        assert!(a.wants_next_block());
    }
}
