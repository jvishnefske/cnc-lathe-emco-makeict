//! The browser half of the kd240 link: `EventSource` for the status
//! channel, `fetch` for commands. Requests go one at a time, in order (the
//! service applies them in arrival order, and the screen relies on a
//! `singleBlock` reaching the session before the `run` that follows it).
//!
//! All interpretation is in the parent module; this only moves strings.

use super::{calls_for, interpret, Decoder, HttpCall, CAPABILITIES};
use crate::link::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{EventSource, MessageEvent};

struct Shared {
    inbox: VecDeque<Incoming>,
    queue: VecDeque<HttpCall>,
    busy: bool,
    token: Option<String>,
    decoder: Decoder,
    repaint: Box<dyn Fn()>,
}

/// The kd240 service, from a page it served (same origin).
pub struct Kd240Link {
    shared: Rc<RefCell<Shared>>,
    _events: EventSource,
}

fn push(shared: &Rc<RefCell<Shared>>, items: Vec<Incoming>) {
    let mut s = shared.borrow_mut();
    s.inbox.extend(items);
    (s.repaint)();
}

fn listen(
    es: &EventSource,
    name: &str,
    shared: &Rc<RefCell<Shared>>,
    f: fn(&mut Decoder, &str) -> Vec<Incoming>,
) {
    let shared = shared.clone();
    let cb = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let data = e.data().as_string().unwrap_or_default();
        let items = f(&mut shared.borrow_mut().decoder, &data);
        push(&shared, items);
    });
    // The link lives as long as the page, and so do its listeners.
    let _ = es.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
    cb.forget();
}

impl Kd240Link {
    /// Opens `GET /api/events`; `repaint` is called whenever something
    /// arrives.
    pub fn open(repaint: Box<dyn Fn()>) -> Result<Kd240Link, JsValue> {
        let shared = Rc::new(RefCell::new(Shared {
            inbox: VecDeque::from([Incoming::Link(LinkState::Connecting)]),
            queue: VecDeque::new(),
            busy: false,
            token: None,
            decoder: Decoder::default(),
            repaint,
        }));
        let es = EventSource::new("/api/events")?;
        listen(
            &es,
            "state",
            &shared,
            |d, data| match serde_json::from_str(data) {
                Ok(v) => vec![Incoming::Status(Box::new(d.state(&v)))],
                Err(e) => vec![bad_event("state", e)],
            },
        );
        listen(&es, "core", &shared, |d, data| {
            match serde_json::from_str(data) {
                Ok(v) => d.core(&v),
                Err(e) => vec![bad_event("core", e)],
            }
        });
        listen(&es, "lagged", &shared, |d, data| {
            d.lagged(data.trim().parse().unwrap_or(0))
        });
        {
            let s = shared.clone();
            let open = Closure::<dyn FnMut()>::new(move || {
                push(&s, vec![Incoming::Link(LinkState::Connected)])
            });
            es.set_onopen(Some(open.as_ref().unchecked_ref()));
            open.forget();
            let s = shared.clone();
            let error = Closure::<dyn FnMut()>::new(move || {
                // EventSource reconnects by itself.
                push(
                    &s,
                    vec![Incoming::Link(LinkState::Lost(
                        "event stream interrupted; reconnecting".into(),
                    ))],
                )
            });
            es.set_onerror(Some(error.as_ref().unchecked_ref()));
            error.forget();
        }
        Ok(Kd240Link {
            shared,
            _events: es,
        })
    }
}

fn bad_event(name: &str, e: serde_json::Error) -> Incoming {
    Incoming::Error(ErrorMsg {
        kind: ErrorKind::Link,
        text: format!("unreadable {name} event: {e}"),
    })
}

fn pump(shared: Rc<RefCell<Shared>>) {
    let (call, token) = {
        let mut s = shared.borrow_mut();
        if s.busy {
            return;
        }
        let Some(call) = s.queue.pop_front() else {
            return;
        };
        s.busy = true;
        (call, s.token.clone())
    };
    wasm_bindgen_futures::spawn_local(async move {
        let items = match fetch(&call, token.as_deref()).await {
            Ok((status, body)) => interpret(&call.kind, status, &body),
            Err(_) => interpret(&call.kind, 0, ""),
        };
        push(&shared, items);
        shared.borrow_mut().busy = false;
        pump(shared);
    });
}

async fn fetch(call: &HttpCall, token: Option<&str>) -> Result<(u16, String), JsValue> {
    let init = web_sys::RequestInit::new();
    init.set_method(call.method);
    let headers = web_sys::Headers::new()?;
    if let Some(ct) = call.content_type {
        headers.set("Content-Type", ct)?;
    }
    if call.changes() {
        if let Some(t) = token.filter(|t| !t.is_empty()) {
            headers.set("Authorization", &format!("Bearer {t}"))?;
        }
    }
    init.set_headers(&headers);
    if let Some(body) = &call.body {
        init.set_body(&JsValue::from_str(body));
    }
    let req = web_sys::Request::new_with_str_and_init(&call.path, &init)?;
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&req))
        .await?
        .dyn_into()?;
    let text = JsFuture::from(resp.text()?)
        .await?
        .as_string()
        .unwrap_or_default();
    Ok((resp.status(), text))
}

impl MachineLink for Kd240Link {
    fn send(&mut self, req: Request) {
        let diameter_mode = self.shared.borrow().decoder.diameter_mode();
        match calls_for(&req, diameter_mode) {
            Ok(calls) => self.shared.borrow_mut().queue.extend(calls),
            Err(e) => push(
                &self.shared,
                vec![Incoming::Error(ErrorMsg {
                    kind: ErrorKind::Operator,
                    text: format!("not sent: {e}"),
                })],
            ),
        }
        pump(self.shared.clone());
    }

    fn poll(&mut self) -> Vec<Incoming> {
        self.shared.borrow_mut().inbox.drain(..).collect()
    }

    fn capabilities(&self) -> Capabilities {
        CAPABILITIES
    }

    fn set_token(&mut self, token: Option<String>) {
        self.shared.borrow_mut().token = token;
    }
}
