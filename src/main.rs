mod lyrics;
mod spotify;
mod spotify_official;
mod storage;
mod ui;

use anyhow::Result;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static APP_CONTROLLER: RefCell<Option<Rc<ui::AppController>>> = const { RefCell::new(None) };
}

fn main() -> Result<()> {
    let app = gtk::Application::builder()
        .application_id("io.github.kazu.spotify_lyrics")
        .build();

    app.connect_activate(|app| match ui::AppController::new(app) {
        Ok(controller) => {
            controller.show();
            APP_CONTROLLER.with(|slot| {
                *slot.borrow_mut() = Some(controller);
            });
        }
        Err(err) => {
            eprintln!("failed to start: {err:#}");
            app.quit();
        }
    });

    app.run();
    Ok(())
}
