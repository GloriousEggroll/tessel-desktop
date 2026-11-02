//! `applet_pages._datetime`: the calendar's Date & Time settings.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tessel_calendar::backend::DateSystem;
use tessel_calendar::settings::DateSettingsWindow;
use gs_base::{Host, SettingsPage};
use gtk::glib;

use crate::embed::{embed, Embed};
use crate::Factories;

/// Seconds between clock refreshes while the page is visible.
pub const TICK_SECONDS: u32 = 15;

/// `page.controller` / `page.backend` of the Date & Time page.
pub struct DatePage {
    pub controller: DateSettingsWindow,
    pub backend: Rc<dyn DateSystem>,
    timer: Rc<RefCell<Option<glib::SourceId>>>,
}

impl DatePage {
    /// True while the 15 s refresh timer runs (only between activate and deactivate).
    pub fn ticking(&self) -> bool {
        self.timer.borrow().is_some()
    }
}

fn register_targets(page: &SettingsPage, view: &DateSettingsWindow) {
    let s = &view.0;
    let targets: [(&str, gtk::Widget); 11] = [
        ("ntp_switch", s.ntp_switch.clone().into()), ("timezone_entry", s.timezone_entry.clone().into()),
        ("hour_spin", s.hour_spin.clone().into()), ("clock_stack_switch", s.clock_stack_switch.clone().into()),
        ("clock_show_date_switch", s.clock_show_date_switch.clone().into()),
        ("clock_show_year_switch", s.clock_show_year_switch.clone().into()),
        ("clock_show_weekday_switch", s.clock_show_weekday_switch.clone().into()),
        ("clock_reverse_switch", s.clock_reverse_switch.clone().into()),
        ("clock_hour_combo", s.clock_hour_combo.clone().into()), ("clock_date_combo", s.clock_date_combo.clone().into()),
        ("clock_separator_combo", s.clock_separator_combo.clone().into()),
    ];
    for (name, widget) in targets {
        page.add_target(name, &widget);
    }
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let application = crate::application(host)?;
    let view: Rc<RefCell<Option<DateSettingsWindow>>> = Rc::default();
    let changed: Rc<dyn Fn()> = {
        let view = view.clone();
        Rc::new(move || {
            let current = view.borrow().clone();
            if let Some(view) = current {
                view.refresh();
            }
        })
    };
    let failed: Rc<dyn Fn(&str)> = {
        let changed = changed.clone();
        Rc::new(move |_message: &str| changed())
    };
    let backend: Rc<dyn DateSystem> = match &factories.datetime {
        Some(factory) => factory(changed.clone(), failed),
        None => {
            let (changed, failed) = (changed.clone(), failed.clone());
            Rc::new(tessel_calendar::backend::DateBackend::new(move || changed(), move |message| failed(message)))
        }
    };
    let window = DateSettingsWindow::new(&application, backend.clone());
    view.replace(Some(window.clone()));
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let deactivate: Rc<dyn Fn()> = {
        let timer = timer.clone();
        Rc::new(move || {
            if let Some(source) = timer.take() {
                source.remove();
            }
        })
    };
    let activate = {
        let (timer, changed) = (timer.clone(), changed.clone());
        move || {
            changed();
            if timer.borrow().is_none() {
                let tick = changed.clone();
                let source = glib::timeout_add_seconds_local(TICK_SECONDS, move || {
                    tick();
                    glib::ControlFlow::Continue
                });
                timer.replace(Some(source));
            }
        }
    };
    let cleanup = {
        let (deactivate, view, backend) = (deactivate.clone(), view.clone(), backend.clone());
        let done = Rc::new(Cell::new(false));
        move || {
            if done.replace(true) {
                return;
            }
            deactivate();
            if let Some(view) = view.take() {
                view.destroyed();
            }
            backend.close();
        }
    };
    let page = embed(window.window(), host, Embed {
        refresh: Some(changed), cleanup: Some(Box::new(cleanup)), deactivate: Some(deactivate), ..Default::default()
    });
    page.set_activate(activate);
    register_targets(&page, &window);
    page.set_controller(Rc::new(DatePage { controller: window, backend: backend.clone(), timer }));
    backend.start();
    Ok(page)
}
