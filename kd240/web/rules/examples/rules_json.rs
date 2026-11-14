//! Answers rule queries as JSON lines, for `diff_against_python.py`, which
//! asks `emco_rules.py` the same questions and compares the answers.
//!
//! Each input line is one of
//!   {"preflight": text, "belt": n, "machine": [x_retract, z_park, tool_change_retracts]}
//!   {"spindle": rpm, "belt": n, "threading": bool}
//!   {"clamp": rpm, "belt": n}
//!   {"gate": [[estop, machine_on, [homed, ...]], ...], "belt": n}
//! and each output line is the answer, shaped as the Python script prints it.
use emco_rules::{clamp_rpm, preflight, spindle_violation, Belt, Machine, ReferenceGate};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let q: Value = serde_json::from_str(&line.expect("stdin")).expect("a JSON line");
        let belt = Belt::new(q["belt"].as_u64().expect("belt") as u8).expect("belt 1-3");
        let a = if let Some(text) = q["preflight"].as_str() {
            let m = &q["machine"];
            let machine = Machine {
                x_retract: m[0].as_f64().unwrap(),
                z_park: m[1].as_f64().unwrap(),
                tool_change_retracts: m[2].as_bool().unwrap(),
            };
            json!(preflight(text, belt, &machine)
                .iter()
                .map(|f| json!([f.line, f.severity.as_str(), f.message]))
                .collect::<Vec<_>>())
        } else if let Some(rpm) = q["spindle"].as_f64() {
            json!(spindle_violation(
                belt,
                rpm,
                q["threading"].as_bool().unwrap()
            ))
        } else if let Some(steps) = q["gate"].as_array() {
            let mut g = ReferenceGate::new();
            json!(steps
                .iter()
                .map(|s| {
                    let homed: Vec<bool> = s[2]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|h| h.as_bool().unwrap())
                        .collect();
                    json!([
                        g.update(s[0].as_bool().unwrap(), s[1].as_bool().unwrap(), &homed),
                        g.position_lost()
                    ])
                })
                .collect::<Vec<_>>())
        } else {
            json!(clamp_rpm(belt, q["clamp"].as_f64().unwrap()))
        };
        writeln!(out, "{a}").unwrap();
    }
}
