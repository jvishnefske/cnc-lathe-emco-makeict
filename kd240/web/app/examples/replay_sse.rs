//! Replays a captured `GET /api/events` stream through the kd240 adapter
//! and the screen's logic, printing what the screen would show: the gate
//! banner, run state, log lines, and the requests it would send by itself.
//!
//!   curl -sN http://127.0.0.1:8240/api/events > events.txt   # while operating
//!   cargo run -p emco-egui --example replay_sse -- events.txt
//!
//! Time is the events' own `atMs`, so the proving ladder's delay behaves as
//! it would have.
use emco_egui::kd240::{Decoder, CAPABILITIES};
use emco_egui::link::*;
use emco_egui::screen::{MemoryPrefs, Screen};
use serde_json::Value;

#[derive(Default)]
struct Recorder(Vec<Request>, Vec<Incoming>);

impl MachineLink for Recorder {
    fn send(&mut self, req: Request) {
        self.0.push(req);
    }
    fn poll(&mut self) -> Vec<Incoming> {
        std::mem::take(&mut self.1)
    }
    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }
    fn set_token(&mut self, _: Option<String>) {}
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: replay_sse <events.txt>");
    let text = std::fs::read_to_string(&path).expect("read the capture");
    let mut prefs = MemoryPrefs::default();
    let mut screen = Screen::new(&prefs, CAPABILITIES);
    let mut link = Recorder::default();
    let mut decoder = Decoder::default();
    screen.apply(Incoming::Link(LinkState::Connected), &mut link);
    link.0.clear(); // the startup requests
    let (mut banner, mut run, mut logged) = (String::new(), String::new(), 0);
    let mut now = 0.0;
    for block in text.split("\n\n") {
        let (mut event, mut data) = ("", String::new());
        for line in block.lines() {
            if let Some(e) = line.strip_prefix("event: ") {
                event = e;
            } else if let Some(d) = line.strip_prefix("data: ") {
                data.push_str(d);
            }
        }
        let items = match event {
            "state" => vec![Incoming::Status(Box::new(
                decoder.state(&serde_json::from_str(&data).unwrap()),
            ))],
            "core" => {
                let v: Value = serde_json::from_str(&data).unwrap();
                now = v["atMs"].as_f64().unwrap_or(now);
                decoder.core(&v)
            }
            "lagged" => decoder.lagged(data.trim().parse().unwrap_or(0)),
            _ => continue,
        };
        link.1 = items;
        screen.update(now, &mut link, &mut prefs);
        let b = emco_rules::screen::banner(screen.gate_reason).to_string();
        let st = screen.status.as_ref().unwrap();
        let r = format!(
            "{} line {:?} estop {} on {} homed {:?} dia {}",
            st.run.name(),
            st.current_line.map(|l| l + 1),
            st.estop,
            st.machine_on,
            st.homed,
            if st.diameter_mode { "G7" } else { "G8" }
        );
        if b != banner {
            println!("{now:>10.0} BANNER  {b}");
            banner = b;
        }
        if r != run {
            println!("{now:>10.0} STATUS  {r}");
            run = r;
        }
        for l in screen.log.iter().skip(logged) {
            println!("{now:>10.0} LOG     {l}");
        }
        logged = screen.log.len();
        for q in link.0.drain(..) {
            println!("{now:>10.0} SENDS   {q:?}");
        }
    }
    screen.update(now + 1000.0, &mut link, &mut prefs);
    for l in screen.log.iter().skip(logged) {
        println!("{now:>10.0} LOG     {l}");
    }
    println!("belt question asked: {}", screen.ask_belt);
}
