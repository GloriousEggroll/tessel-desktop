//! `language_page.py`: the same language wizard, including its
//! authentication/rollback flow, embedded as a page.
use std::rc::Rc;
use std::sync::Arc;

use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use tessel_language::ui::{LanguageWizard, WizardBackend};

use crate::Factories;

/// `page.controller` is the embedded [`LanguageWizard`] itself (Welcome drives
/// its steps through it); this alias documents that.
pub type LanguagePage = LanguageWizard;

/// `EmbeddedWizard.close`: a page never closes; a finished change returns
/// to the first step and reloads the current language.
fn embedded_close(wizard: &LanguageWizard) {
    if wizard.busy.get() {
        return;
    }
    let reload = wizard.page.get() == "result";
    wizard.show_page("choose");
    wizard.message("");
    if reload {
        wizard.spinner.start();
        wizard.progress_row.set_visible(true);
        let backend = wizard.backend.clone();
        let weak = wizard.downgrade();
        wizard.run(move || backend.status(), move |result| {
            if let Some(inner) = weak.upgrade() {
                LanguageWizard(inner).loaded(result);
            }
        });
    }
}

pub(crate) fn build(host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let backend: Arc<dyn WizardBackend> = match &factories.language {
        Some(factory) => factory(),
        None => Arc::new(tessel_language::backend::LanguageBackend::default()),
    };
    let application = host.application();
    let controller = LanguageWizard::new(application.as_ref(), backend);
    let weak = controller.downgrade();
    controller.close_override.replace(Some(Rc::new(move || {
        if let Some(inner) = weak.upgrade() {
            embedded_close(&LanguageWizard(inner));
        }
    })));
    let content = controller.window.child().ok_or("The language wizard has no content")?;
    controller.window.set_child(None::<&gtk::Widget>);
    content.add_css_class("languagemanager");
    controller.cancel_button.set_visible(false);
    controller.window.set_transient_for(Some(&host.window()));
    // The normal window was never realized, so explicitly release its workers.
    let closing = controller.clone();
    let asking = controller.clone();
    let page = SettingsPage::new(content)
        .with_close(move || {
            closing.unrealized();
            (closing.language_style_close)();
            closing.window.destroy();
        })
        .with_can_close(move || !asking.close_requested());
    page.set_controller(Rc::new(controller));
    Ok(page)
}
