//! The browser: eframe on a canvas, `localStorage` for preferences, and a
//! file picker for uploads.

use crate::kd240::web::Kd240Link;
use crate::link::MachineLink;
use crate::screen::{MemoryPrefs, Prefs, Screen};
use crate::view::{self, FilePicker};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// `localStorage`, best effort: a private window or a refused storage
/// falls back to memory for this page's life.
struct LocalStoragePrefs {
    fallback: MemoryPrefs,
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

impl Prefs for LocalStoragePrefs {
    fn get(&self, key: &str) -> Option<String> {
        storage()
            .and_then(|s| s.get_item(key).ok().flatten())
            .or_else(|| self.fallback.get(key))
    }
    fn set(&mut self, key: &str, value: &str) {
        if storage()
            .and_then(|s| s.set_item(key, value).ok())
            .is_none()
        {
            self.fallback.set(key, value);
        }
    }
    fn remove(&mut self, key: &str) {
        if let Some(s) = storage() {
            let _ = s.remove_item(key);
        }
        self.fallback.remove(key);
    }
}

/// A hidden `<input type=file>`; the chosen file's text arrives later.
struct BrowserFiles {
    picked: Rc<RefCell<Option<(String, String)>>>,
    repaint: egui::Context,
}

impl FilePicker for BrowserFiles {
    fn available(&self) -> bool {
        true
    }

    fn open(&mut self) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(input) = doc.create_element("input") else {
            return;
        };
        let Ok(input) = input.dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        input.set_type("file");
        input.set_accept(".nc,.ngc,.tap,.txt,text/plain");
        let picked = self.picked.clone();
        let ctx = self.repaint.clone();
        let el = input.clone();
        let on_change = Closure::<dyn FnMut()>::new(move || {
            let Some(file) = el.files().and_then(|f| f.get(0)) else {
                return;
            };
            let name = file.name();
            let picked = picked.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(text) = wasm_bindgen_futures::JsFuture::from(file.text()).await {
                    *picked.borrow_mut() = Some((name, text.as_string().unwrap_or_default()));
                    ctx.request_repaint();
                }
            });
        });
        input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        on_change.forget();
        input.click();
    }

    fn take(&mut self) -> Option<(String, String)> {
        self.picked.borrow_mut().take()
    }
}

struct App {
    screen: Screen,
    link: Kd240Link,
    prefs: LocalStoragePrefs,
    files: BrowserFiles,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now_ms = ctx.input(|i| i.time) * 1000.0;
        self.screen.update(now_ms, &mut self.link, &mut self.prefs);
        view::show(
            ctx,
            &mut self.screen,
            &mut self.link,
            &mut self.prefs,
            &mut self.files,
        );
        // The proving ladder's finish delay runs on this clock.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// Starts the screen on `<canvas id="emco_canvas">`.
#[wasm_bindgen(start)]
pub fn start() {
    wasm_bindgen_futures::spawn_local(async {
        let result = run().await;
        if let Err(e) = result {
            let msg = e.as_string().unwrap_or_else(|| format!("{e:?}"));
            if let Some(el) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id("status"))
            {
                el.set_text_content(Some(&format!("The screen did not start: {msg}")));
            }
        }
    });
}

async fn run() -> Result<(), JsValue> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let canvas = doc
        .get_element_by_id("emco_canvas")
        .ok_or("no canvas #emco_canvas")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| {
                let ctx = cc.egui_ctx.clone();
                ctx.set_visuals(egui::Visuals::dark());
                let repaint = ctx.clone();
                let link = Kd240Link::open(Box::new(move || repaint.request_repaint()))
                    .map_err(|e| format!("event stream: {e:?}"))?;
                let prefs = LocalStoragePrefs {
                    fallback: MemoryPrefs::default(),
                };
                let screen = Screen::new(&prefs, link.capabilities());
                if let Some(el) = doc.get_element_by_id("status") {
                    el.remove();
                }
                Ok(Box::new(App {
                    screen,
                    link,
                    prefs,
                    files: BrowserFiles {
                        picked: Rc::new(RefCell::new(None)),
                        repaint: ctx,
                    },
                }))
            }),
        )
        .await
}
