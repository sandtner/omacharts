//! One alert, to set or to change: what it is on, what the price has to do,
//! how often it fires, until when, and what it says.
//!
//! Unlike a drawing's properties this does not apply as it goes. An alert
//! half-edited is an alert that fires on the wrong level, so nothing is
//! written until Save — or Ctrl+Enter — and Cancel or Escape leave it as it
//! was. Saving arms it again, the way saving a stopped alert does anywhere
//! else: an edit is somebody saying what they now want to be told.

use std::rc::Rc;

use adw::prelude::*;
use omacharts_engine::alert::{next_id, Alert, Condition, Frequency};
use omacharts_engine::price_decimals;

use crate::ui::dialogs;
use crate::ui::window::Window;

/// How long a new alert runs when "Expires" is switched on: a month, as a
/// starting point to type over.
const DEFAULT_EXPIRY_SECONDS: i64 = 30 * 86_400;

/// A new alert on the focused chart's symbol, at `price` or at its last
/// price, crossing. Nothing opens without a symbol on the chart.
pub fn present_new(window: &Rc<Window>, price: Option<f64>) {
    let pane = window.focused_pane();
    let instrument = pane.instrument.borrow().clone();
    let Some(instrument) = instrument else { return };
    let last = crate::cli::quote(window.store(), window.provider().as_ref(), &instrument).map(|q| q.last);
    let Some(level) = price.or(last) else { return };
    let mut draft = Alert::new(
        &instrument.symbol,
        instrument.suffix.as_deref(),
        Condition::Cross,
        level,
        chrono::Utc::now().timestamp(),
    );
    // Rounded to what the symbol is quoted in: 195.3271 from a click is a
    // level nobody meant.
    let decimals = price_decimals(0.0, level, Some(instrument.kind));
    draft.level = round(level, decimals);
    present(window, draft);
}

/// Open the editor on `draft`: a new alert when its id is 0, else the
/// stored alert with that id.
pub fn present(window: &Rc<Window>, draft: Alert) {
    let index = window.symbols();
    let find = {
        let index = index.clone();
        move |symbol: &str, suffix: Option<&str>| index.as_ref().and_then(|i| i.find(symbol, suffix).cloned())
    };
    let instrument = find(&draft.symbol, draft.suffix.as_deref());
    let kind = instrument.as_ref().map(|i| i.kind);
    let decimals = price_decimals(0.0, draft.level, kind);
    let last = instrument
        .as_ref()
        .and_then(|i| crate::cli::quote(window.store(), window.provider().as_ref(), i))
        .map(|q| q.last);
    let is_new = draft.id == 0;
    let on_line = draft.drawing;

    // -- What it is on, and what the price has to do ----------------------
    let condition_group = adw::PreferencesGroup::new();
    condition_group.set_title("Condition");

    let symbol = adw::EntryRow::new();
    symbol.set_title("Symbol");
    symbol.set_text(&draft.spell_symbol());
    symbol.set_sensitive(on_line.is_none());
    condition_group.add(&symbol);

    // A line decides the level; it can still be crossed one way or either,
    // or sat above or below. A channel needs two prices of its own.
    let conditions: Vec<Condition> = Condition::ALL
        .into_iter()
        .filter(|c| on_line.is_none() || !c.is_channel())
        .collect();
    let condition = adw::ComboRow::new();
    condition.set_title("Condition");
    let labels: Vec<&str> = conditions.iter().map(|c| c.label()).collect();
    condition.set_model(Some(&gtk::StringList::new(&labels)));
    condition.set_selected(conditions.iter().position(|c| *c == draft.condition).unwrap_or(0) as u32);
    condition_group.add(&condition);

    let step = 10f64.powi(-(decimals as i32));
    let spin = |title: &str, value: f64| {
        let row = adw::SpinRow::with_range(0.0, 1e12, step);
        row.set_title(title);
        row.set_digits(decimals as u32);
        row.set_value(value);
        row
    };
    let (lo, hi) = draft.bounds();
    let channel_now = draft.condition.is_channel();
    let level = spin("Price", if channel_now { lo } else { draft.level });
    let upper = spin("Upper bound", draft.upper.map_or_else(|| suggest_upper(draft.level, last), |_| hi));
    if let Some(line) = on_line {
        level.set_sensitive(false);
        level.set_subtitle(&format!("Follows horizontal line #{line}: move the line to move the alert"));
    }
    condition_group.add(&level);
    condition_group.add(&upper);

    // The price now, and the way to put it in the level.
    let now_row = adw::ActionRow::new();
    now_row.set_title("Last price");
    now_row.set_subtitle(&match last {
        Some(price) => format!("{price:.decimals$}"),
        None => "Not known yet".into(),
    });
    now_row.add_css_class("property");
    if let Some(price) = last.filter(|_| on_line.is_none()) {
        let use_it = gtk::Button::with_label("Use");
        use_it.add_css_class("flat");
        use_it.set_valign(gtk::Align::Center);
        use_it.set_tooltip_text(Some("Put the last price in the level"));
        let level = level.clone();
        use_it.connect_clicked(move |_| level.set_value(round(price, decimals)));
        now_row.add_suffix(&use_it);
    }
    condition_group.add(&now_row);

    // -- How often, and until when -----------------------------------------
    let trigger_group = adw::PreferencesGroup::new();
    trigger_group.set_title("Trigger");
    let frequency = adw::ComboRow::new();
    frequency.set_title("Fire");
    let labels: Vec<&str> = Frequency::ALL.iter().map(|f| f.label()).collect();
    frequency.set_model(Some(&gtk::StringList::new(&labels)));
    frequency.set_selected(Frequency::ALL.iter().position(|f| *f == draft.frequency).unwrap_or(0) as u32);
    trigger_group.add(&frequency);

    let expires = adw::SwitchRow::new();
    expires.set_title("Expires");
    expires.set_active(draft.expires.is_some());
    trigger_group.add(&expires);
    let expiry = adw::EntryRow::new();
    expiry.set_title("Until (2026-12-31 16:00, or 3d, 2w)");
    let now = chrono::Utc::now().timestamp();
    expiry.set_text(&crate::alerts::spell_expiry(draft.expires.unwrap_or(now + DEFAULT_EXPIRY_SECONDS)));
    expires.bind_property("active", &expiry, "visible").sync_create().build();
    trigger_group.add(&expiry);

    // -- What it says --------------------------------------------------------
    let message_group = adw::PreferencesGroup::new();
    message_group.set_title("Notification");
    message_group.set_description(Some("In the message, {{ticker}}, {{price}} and {{level}} are filled in as it fires."));
    let name = adw::EntryRow::new();
    name.set_title("Name");
    name.set_text(&draft.name);
    message_group.add(&name);
    let note = adw::EntryRow::new();
    note.set_title("Message");
    note.set_text(&draft.note);
    message_group.add(&note);
    let preview = adw::ActionRow::new();
    preview.set_title("Preview");
    preview.add_css_class("property");
    preview.set_subtitle_lines(3);
    message_group.add(&preview);

    let page = adw::PreferencesPage::new();
    page.add(&condition_group);
    page.add(&trigger_group);
    page.add(&message_group);

    let dialog = adw::Dialog::new();
    dialog.set_title(&match is_new {
        true => "New alert".to_string(),
        false => format!("Edit alert #{}", draft.id),
    });
    dialog.set_content_width(480);
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_show_start_title_buttons(false);
    let cancel = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel);
    let save = gtk::Button::with_label(if is_new { "Create" } else { "Save" });
    save.add_css_class("suggested-action");
    save.set_tooltip_text(Some(&dialogs::commit_label()));
    header.pack_end(&save);
    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&page));
    dialog.set_child(Some(&root));

    // What the rows say, as an alert: the draft with them put on it. An
    // error names the row it is about, so that row can be marked.
    let read = {
        let (symbol, condition, level, upper, frequency, expires, expiry, name, note) = (
            symbol.clone(),
            condition.clone(),
            level.clone(),
            upper.clone(),
            frequency.clone(),
            expires.clone(),
            expiry.clone(),
            name.clone(),
            note.clone(),
        );
        let conditions = conditions.clone();
        let draft = draft.clone();
        let index = index.clone();
        Rc::new(move || -> Result<Alert, (gtk::Widget, String)> {
            let mut alert = draft.clone();
            if on_line.is_none() {
                let text = symbol.text();
                let Some((sym, suffix)) =
                    index.as_ref().and_then(|i| crate::alerts::resolve_symbol(i, &text))
                else {
                    return Err((symbol.clone().upcast(), format!("{text:?} is not a symbol this app knows")));
                };
                if !alert.is_for(&sym, suffix.as_deref()) {
                    alert.symbol = sym;
                    alert.suffix = suffix;
                    alert.last = None;
                }
            }
            alert.condition = conditions.get(condition.selected() as usize).copied().unwrap_or(Condition::Cross);
            if on_line.is_none() {
                alert.level = level.value();
                if alert.level <= 0.0 {
                    return Err((level.clone().upcast(), "A price is more than zero".into()));
                }
            }
            alert.upper = None;
            if alert.condition.is_channel() {
                let other = upper.value();
                if other <= 0.0 || other == alert.level {
                    return Err((upper.clone().upcast(), "A channel needs two different prices".into()));
                }
                alert.upper = Some(other);
            }
            alert.frequency = Frequency::ALL.get(frequency.selected() as usize).copied().unwrap_or_default();
            alert.expires = match expires.is_active() {
                false => None,
                true => crate::alerts::parse_expiry(&expiry.text(), chrono::Utc::now().timestamp())
                    .map_err(|e| (expiry.clone().upcast::<gtk::Widget>(), e))?,
            };
            alert.name = name.text().trim().to_string();
            alert.note = note.text().to_string();
            Ok(alert)
        })
    };

    // The rows that only mean something for one kind of condition, and the
    // preview of the notification, follow what is chosen.
    let sync: Rc<dyn Fn()> = {
        let (condition, level, upper, preview, read) =
            (condition.clone(), level.clone(), upper.clone(), preview.clone(), read.clone());
        let conditions = conditions.clone();
        Rc::new(move || {
            let chosen = conditions.get(condition.selected() as usize).copied().unwrap_or(Condition::Cross);
            upper.set_visible(chosen.is_channel());
            if on_line.is_none() {
                level.set_title(if chosen.is_channel() { "Lower bound" } else { "Price" });
            }
            match read() {
                Ok(alert) => {
                    let price = last.unwrap_or(alert.level);
                    preview.set_subtitle(&glib_escape(&preview_text(&alert, price, decimals)));
                }
                Err((_, why)) => preview.set_subtitle(&glib_escape(&why)),
            }
        })
    };
    {
        let sync = sync.clone();
        condition.connect_selected_notify(move |_| sync());
    }
    for spin in [&level, &upper] {
        let sync = sync.clone();
        spin.connect_value_notify(move |_| sync());
    }
    for entry in [&symbol, &name, &note, &expiry] {
        let sync = sync.clone();
        entry.connect_changed(move |_| sync());
    }
    sync();

    let commit: Rc<dyn Fn()> = {
        let window = window.clone();
        let dialog = dialog.clone();
        let read = read.clone();
        let rows: Vec<gtk::Widget> =
            vec![symbol.clone().upcast(), level.clone().upcast(), upper.clone().upcast(), expiry.clone().upcast()];
        Rc::new(move || {
            for row in &rows {
                row.remove_css_class("error");
            }
            let mut alert = match read() {
                Ok(alert) => alert,
                Err((row, _)) => {
                    row.add_css_class("error");
                    row.grab_focus();
                    return;
                }
            };
            let store = window.store();
            let mut alerts = store.alerts();
            alert.fired = None;
            if alert.last.is_none() {
                alert.last = window
                    .symbols()
                    .and_then(|i| i.find(&alert.symbol, alert.suffix.as_deref()).cloned())
                    .and_then(|i| crate::cli::quote(store, window.provider().as_ref(), &i))
                    .map(|q| q.last);
            }
            match alerts.iter_mut().find(|a| a.id == alert.id && alert.id != 0) {
                Some(stored) => {
                    // The log and anything the checks wrote since the editor
                    // opened stay; only what the rows say is taken.
                    alert.log = std::mem::take(&mut stored.log);
                    *stored = alert;
                }
                None => {
                    alert.id = next_id(&alerts);
                    alert.created = chrono::Utc::now().timestamp();
                    alerts.push(alert);
                }
            }
            store.set_alerts(&alerts);
            window.refresh_alert_levels();
            dialog.close();
        })
    };
    {
        let commit = commit.clone();
        save.connect_clicked(move |_| commit());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    dialogs::commit_on_ctrl_enter(&dialog, move || commit());
    dialog.present(Some(&window.window));
    if on_line.is_none() && is_new {
        level.grab_focus();
    }
}

/// A channel's other bound, to start from: the same distance on the far side
/// of the price, or two percent above the level.
fn suggest_upper(level: f64, last: Option<f64>) -> f64 {
    match last {
        Some(last) if (last - level).abs() > f64::EPSILON => last + (last - level),
        _ => level * 1.02,
    }
}

fn round(price: f64, decimals: usize) -> f64 {
    let scale = 10f64.powi(decimals as i32);
    (price * scale).round() / scale
}

/// A row's subtitle is markup; a message is not.
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// What the notification will say when it fires, with the last price
/// standing in for the one that will fire it.
fn preview_text(alert: &Alert, price: f64, decimals: usize) -> String {
    let what = format!(
        "{} {} {}",
        alert.spell_symbol(),
        alert.condition.label().to_lowercase(),
        alert.spell_level(decimals)
    );
    let title = match alert.name.is_empty() {
        true => what,
        false => format!("{}: {what}", alert.name),
    };
    let message = alert.message(price, decimals);
    let body = match message.is_empty() {
        true => format!("now {price:.decimals$}"),
        false => format!("now {price:.decimals$} · {message}"),
    };
    format!("{title}\n{body}")
}
