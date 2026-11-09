//! The alerts, as a manager: every alert in a table — what it is on, what it
//! fires on, how far the price is from it, how often it fires, until when,
//! and whether it is on — and a log of when each fired.
//!
//! A grid rather than a list of rows, because the point of looking is to
//! compare — which is nearest, which have gone off — and that is a glance down
//! a column. The switch at the head of a row is whether the alert is on:
//! off pauses it, on resumes it, and on for one that has fired arms it again.
//!
//! The open manager is redrawn whenever the alerts change — from a button in
//! it, the editor, a firing, or a command from a terminal — through
//! [`changed`], which the window calls with every change it shows the charts.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use omacharts_engine::alert::{Alert, Frequency, State};
use omacharts_engine::{grouped_decimals, price_decimals};

use crate::ui::alert_editor;
use crate::ui::Window;

type Redraw = Rc<dyn Fn()>;

thread_local! {
    /// The open manager's redraw. A weak hold: the dialog owns the closure,
    /// and a closed dialog leaves nothing here to call.
    static OPEN: RefCell<Option<Weak<dyn Fn()>>> = const { RefCell::new(None) };
}

/// The alerts changed: redraw the manager if it is open.
pub fn changed() {
    let again = OPEN.with(|open| open.borrow().as_ref().and_then(Weak::upgrade));
    if let Some(again) = again {
        // After the handler that changed them has returned: a button
        // removing the row it sits in, while its own click is running, is
        // the kind of thing that works until it does not.
        gtk::glib::idle_add_local_once(move || again());
    }
}

/// Which alerts the table shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Only(State),
}

const FILTERS: [(Filter, &str); 5] = [
    (Filter::All, "All"),
    (Filter::Only(State::Armed), "Active"),
    (Filter::Only(State::Paused), "Paused"),
    (Filter::Only(State::Fired), "Fired"),
    (Filter::Only(State::Expired), "Expired"),
];

/// Write the list and show it everywhere: the charts, the bar widget and,
/// through [`changed`], this.
fn commit(window: &Window, alerts: &[Alert]) {
    window.store().set_alerts(alerts);
    window.refresh_alert_levels();
}

/// Change one alert by its id, and write the list.
fn change(window: &Window, id: i64, f: impl FnOnce(&mut Vec<Alert>, usize)) {
    let mut alerts = window.store().alerts();
    if let Some(at) = alerts.iter().position(|a| a.id == id) {
        f(&mut alerts, at);
        commit(window, &alerts);
    }
}

/// Open the manager.
pub fn present(window: &Rc<Window>) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Alerts");
    dialog.set_content_width(1180);
    dialog.set_content_height(520);

    // -- Header: new, the two pages, and the clearing --------------------
    let header = adw::HeaderBar::new();
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("New alert on the focused chart's symbol (Alt+Shift+A)"));
    header.pack_start(&add);
    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
    header.set_title_widget(Some(&switcher));

    let clearing = gtk::MenuButton::new();
    clearing.set_icon_name("view-more-symbolic");
    clearing.set_tooltip_text(Some("Clear"));
    clearing.set_menu_model(Some(&clear_menu()));
    header.pack_end(&clearing);
    install_clear_actions(window, &dialog);

    // -- Alerts page: search and filter above the table ------------------
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search symbol, name or message"));
    search.set_hexpand(true);
    let filter = Rc::new(Cell::new(Filter::All));
    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    filters.add_css_class("linked");
    let mut buttons: Vec<gtk::ToggleButton> = Vec::new();
    for (n, (_, label)) in FILTERS.iter().enumerate() {
        let button = gtk::ToggleButton::with_label(label);
        if let Some(first) = buttons.first() {
            button.set_group(Some(first));
        }
        button.set_active(n == 0);
        filters.append(&button);
        buttons.push(button);
    }
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bar.set_margin_top(12);
    bar.set_margin_start(18);
    bar.set_margin_end(18);
    bar.append(&search);
    bar.append(&filters);

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&holder));
    let alerts_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    alerts_page.append(&bar);
    alerts_page.append(&scroller);
    stack.add_titled_with_icon(&alerts_page, Some("alerts"), "Alerts", "alarm-symbolic");

    let log_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let log_scroller = gtk::ScrolledWindow::new();
    log_scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    log_scroller.set_vexpand(true);
    log_scroller.set_child(Some(&log_holder));
    stack.add_titled_with_icon(&log_scroller, Some("log"), "Log", "document-open-recent-symbolic");

    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&stack));
    dialog.set_child(Some(&root));

    // -- Drawing it -------------------------------------------------------
    let rebuild: Redraw = {
        let window = window.clone();
        let (holder, log_holder, search, filter) =
            (holder.clone(), log_holder.clone(), search.clone(), filter.clone());
        let buttons = buttons.clone();
        let dialog = dialog.clone();
        Rc::new(move || {
            let alerts = window.store().alerts();
            let now = chrono::Utc::now().timestamp();
            for (button, (which, label)) in buttons.iter().zip(FILTERS) {
                let count = alerts.iter().filter(|a| matches(which, a, now)).count();
                button.set_label(&format!("{label} {count}"));
            }
            while let Some(child) = holder.first_child() {
                holder.remove(&child);
            }
            let needle = search.text().to_lowercase();
            let shown: Vec<&Alert> = alerts
                .iter()
                .filter(|a| matches(filter.get(), a, now))
                .filter(|a| needle.is_empty() || searchable(a).contains(&needle))
                .collect();
            holder.append(&table(&window, &dialog, &alerts, &shown, now));
            while let Some(child) = log_holder.first_child() {
                log_holder.remove(&child);
            }
            log_holder.append(&log_table(&window, &alerts));
        })
    };
    OPEN.with(|open| *open.borrow_mut() = Some(Rc::downgrade(&rebuild)));
    {
        // The dialog holds the redraw; closing it lets it go, and the weak
        // hold above with it.
        let held = RefCell::new(Some(rebuild.clone()));
        dialog.connect_closed(move |_| {
            held.borrow_mut().take();
        });
    }
    for (button, (which, _)) in buttons.iter().zip(FILTERS) {
        let (filter, rebuild) = (filter.clone(), Rc::downgrade(&rebuild));
        button.connect_toggled(move |b| {
            if b.is_active() {
                filter.set(which);
                if let Some(rebuild) = rebuild.upgrade() {
                    rebuild();
                }
            }
        });
    }
    {
        let rebuild = Rc::downgrade(&rebuild);
        search.connect_search_changed(move |_| {
            if let Some(rebuild) = rebuild.upgrade() {
                rebuild();
            }
        });
    }
    crate::ui::dialogs::close_on_search_escape(&search, &dialog);
    {
        let window = window.clone();
        add.connect_clicked(move |_| alert_editor::present_new(&window, None));
    }
    rebuild();
    dialog.present(Some(&window.window));
}

fn matches(filter: Filter, alert: &Alert, now: i64) -> bool {
    match filter {
        Filter::All => true,
        Filter::Only(state) => alert.state(now) == state,
    }
}

fn searchable(alert: &Alert) -> String {
    format!(
        "{} {} {} {} {}",
        alert.spell_symbol(),
        alert.name,
        alert.note,
        alert.spell_condition(),
        alert.rule.as_deref().unwrap_or("")
    )
    .to_lowercase()
}

fn clear_menu() -> gtk::gio::Menu {
    let menu = gtk::gio::Menu::new();
    menu.append(Some("Clear fired alerts"), Some("alerts.clear-fired"));
    menu.append(Some("Clear expired alerts"), Some("alerts.clear-expired"));
    menu.append(Some("Clear the log"), Some("alerts.clear-log"));
    menu
}

fn install_clear_actions(window: &Rc<Window>, dialog: &adw::Dialog) {
    let group = gtk::gio::SimpleActionGroup::new();
    let add = |name: &str, f: fn(&mut Vec<Alert>, i64)| {
        let action = gtk::gio::SimpleAction::new(name, None);
        let window = window.clone();
        action.connect_activate(move |_, _| {
            let mut alerts = window.store().alerts();
            f(&mut alerts, chrono::Utc::now().timestamp());
            commit(&window, &alerts);
        });
        group.add_action(&action);
    };
    add("clear-fired", |alerts, now| alerts.retain(|a| a.state(now) != State::Fired));
    add("clear-expired", |alerts, now| alerts.retain(|a| a.state(now) != State::Expired));
    add("clear-log", |alerts, _| alerts.iter_mut().for_each(|a| a.log.clear()));
    dialog.insert_action_group("alerts", Some(&group));
}

const COLUMNS: [&str; 10] =
    ["", "Symbol", "Alert", "Level", "Last", "Distance", "Trigger", "Expires", "Status", ""];

/// Level, Last and Distance read as figures, right-aligned.
fn numeric(column: usize) -> bool {
    matches!(column, 3..=5)
}

fn table(window: &Rc<Window>, dialog: &adw::Dialog, all: &[Alert], alerts: &[&Alert], now: i64) -> gtk::Widget {
    if alerts.is_empty() {
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("alarm-symbolic"));
        match all.is_empty() {
            true => {
                empty.set_title("No alerts");
                empty.set_description(Some(
                    "Press + or Alt+Shift+A for one on the focused chart, right-click the chart for one at a price, \
                     or right-click a horizontal line.\nFrom a terminal: omacharts alert add AAPL --cross-up 250",
                ));
            }
            false => empty.set_title("Nothing matches"),
        }
        return empty.upcast();
    }

    let grid = gtk::Grid::new();
    grid.set_column_spacing(16);
    grid.set_row_spacing(6);
    grid.set_margin_top(12);
    grid.set_margin_bottom(18);
    grid.set_margin_start(18);
    grid.set_margin_end(18);
    for (column, title) in COLUMNS.iter().enumerate() {
        let label = gtk::Label::new(Some(title));
        label.add_css_class("dim-label");
        label.add_css_class("caption-heading");
        label.set_xalign(if numeric(column) { 1.0 } else { 0.0 });
        grid.attach(&label, column as i32, 0, 1, 1);
    }
    let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
    grid.attach(&rule, 0, 1, COLUMNS.len() as i32, 1);

    // Active first, nearest first; then paused; then what has fired, latest
    // first; expired last. The question the table is opened to answer is
    // "what is about to go off".
    let index = window.symbols();
    let mut rows: Vec<(&Alert, Option<f64>, usize, Option<f64>)> = alerts
        .iter()
        .map(|alert| {
            let instrument = index.as_ref().and_then(|i| i.find(&alert.symbol, alert.suffix.as_deref()));
            let level = level_now(window, alert);
            let last = instrument
                .and_then(|i| window.quote(i))
                .map(|q| q.last)
                .or(alert.last);
            let scale = if alert.is_rule() { last.unwrap_or(alert.scale()) } else { level };
            let decimals = price_decimals(0.0, scale, instrument.map(|i| i.kind));
            let away = if alert.is_rule() { None } else { distance(alert, level, last) };
            (*alert, last, decimals, away)
        })
        .collect();
    let rank = |s: State| match s {
        State::Armed => 0,
        State::Paused => 1,
        State::Fired => 2,
        State::Expired => 3,
    };
    rows.sort_by(|(a, _, _, da), (b, _, _, db)| {
        let (sa, sb) = (a.state(now), b.state(now));
        rank(sa).cmp(&rank(sb)).then_with(|| match sa {
            State::Fired => b.last_fired().cmp(&a.last_fired()),
            _ => da.map_or(f64::MAX, f64::abs).total_cmp(&db.map_or(f64::MAX, f64::abs)),
        })
    });

    for (n, (alert, last, decimals, away)) in rows.into_iter().enumerate() {
        let row = n as i32 + 2;
        let state = alert.state(now);
        let level = level_now(window, alert);
        let price = |p: f64| grouped_decimals(p, decimals);
        let id = alert.id;

        // On or off. Off pauses; on resumes, or arms one that has fired.
        let on = gtk::Switch::new();
        on.set_valign(gtk::Align::Center);
        on.set_active(state == State::Armed);
        on.set_sensitive(state != State::Expired);
        on.set_tooltip_text(Some(match state {
            State::Armed => "On — switch off to pause",
            State::Paused => "Paused — switch on to resume",
            State::Fired => "Fired — switch on to arm it again",
            State::Expired => "Expired — edit it to give it a new date",
        }));
        {
            let window = window.clone();
            on.connect_state_set(move |_, active| {
                change(&window, id, |alerts, at| {
                    let a = &mut alerts[at];
                    match active {
                        true if a.fired.is_some() => a.rearm(),
                        true => a.resume(),
                        false => a.pause(),
                    }
                });
                gtk::glib::Propagation::Proceed
            });
        }
        grid.attach(&on, 0, row, 1, 1);

        // The symbol is the way to its chart.
        let symbol = gtk::Button::with_label(&alert.spell_symbol());
        symbol.add_css_class("flat");
        symbol.set_halign(gtk::Align::Start);
        symbol.set_valign(gtk::Align::Center);
        symbol.set_tooltip_text(Some("Show on the focused chart"));
        if let Some(instrument) =
            window.symbols().and_then(|i| i.find(&alert.symbol, alert.suffix.as_deref()).cloned())
        {
            let (window, dialog) = (window.clone(), dialog.clone());
            symbol.connect_clicked(move |_| {
                window.show(instrument.clone());
                dialog.close();
            });
        } else {
            symbol.set_sensitive(false);
        }
        grid.attach(&symbol, 1, row, 1, 1);

        // Name over what it fires on; the message as the tooltip.
        let what = gtk::Box::new(gtk::Orientation::Vertical, 0);
        what.set_valign(gtk::Align::Center);
        what.set_hexpand(true);
        // A rule is what it fires on, so it stands where a condition does,
        // with the bars it is read on after it.
        let condition = match (&alert.rule, alert.drawing) {
            (Some(rule), _) => format!(
                "{rule} · {}{}",
                alert.rule_timeframe().key(),
                if alert.on_close { ", bar close" } else { "" }
            ),
            (None, Some(line)) => format!("{} · line #{line}", alert.condition.label()),
            (None, None) => alert.spell_condition(),
        };
        let (top, bottom) = match alert.name.is_empty() {
            true => (condition, (!alert.note.is_empty()).then(|| alert.note.clone())),
            false => (alert.name.clone(), Some(condition)),
        };
        let first = gtk::Label::new(Some(&top));
        first.set_xalign(0.0);
        first.set_ellipsize(gtk::pango::EllipsizeMode::End);
        first.set_width_chars(16);
        first.set_max_width_chars(28);
        what.append(&first);
        if let Some(bottom) = bottom {
            let second = gtk::Label::new(Some(&bottom));
            second.set_xalign(0.0);
            second.add_css_class("dim-label");
            second.add_css_class("caption");
            second.set_ellipsize(gtk::pango::EllipsizeMode::End);
            second.set_max_width_chars(28);
            what.append(&second);
        }
        match (&alert.rule, alert.note.is_empty()) {
            (_, false) => what.set_tooltip_text(Some(&alert.note)),
            (Some(rule), true) => what.set_tooltip_text(Some(rule)),
            (None, true) => {}
        }
        grid.attach(&what, 2, row, 1, 1);

        let level_text = match alert.condition.is_channel() {
            _ if alert.is_rule() => "—".to_string(),
            true => {
                let (lo, hi) = alert.bounds();
                format!("{} – {}", price(lo), price(hi))
            }
            false => price(level),
        };
        let away = match (away, alert.condition.is_channel()) {
            _ if alert.is_rule() => "—".to_string(),
            (Some(0.0), true) => "inside".to_string(),
            (Some(d), _) => format!("{d:+.2}%"),
            (None, _) => "—".into(),
        };
        let trigger = match alert.frequency {
            Frequency::Once => "Once".to_string(),
            Frequency::Every => match alert.log.len() {
                0 => "Every time".to_string(),
                n => format!("Every time · {n}×"),
            },
        };
        let expires = alert.expires.map(when).unwrap_or_else(|| "—".into());
        let status = match state {
            State::Armed => "Active".to_string(),
            State::Paused => "Paused".to_string(),
            State::Fired => format!("Fired {}", alert.fired.map(when).unwrap_or_default()),
            State::Expired => "Expired".to_string(),
        };
        let cells = [level_text, last.map(price).unwrap_or_else(|| "—".into()), away, trigger, expires, status];
        for (n, text) in cells.iter().enumerate() {
            let column = n + 3;
            let label = gtk::Label::new(Some(text));
            label.set_xalign(if numeric(column) { 1.0 } else { 0.0 });
            // Never cut short: a level missing its last digits is a
            // different level, and the Alert column gives way instead.
            if numeric(column) {
                label.add_css_class("numeric");
            }

            if column == 8 {
                match state {
                    State::Fired => label.add_css_class("warning"),
                    State::Paused | State::Expired => label.add_css_class("dim-label"),
                    State::Armed => label.add_css_class("success"),
                }
            }
            grid.attach(&label, column as i32, row, 1, 1);
        }

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        actions.set_halign(gtk::Align::End);
        let button = |icon: &str, tip: &str| {
            let b = gtk::Button::from_icon_name(icon);
            b.add_css_class("flat");
            b.set_tooltip_text(Some(tip));
            actions.append(&b);
            b
        };
        if state == State::Fired {
            let window = window.clone();
            button("view-refresh-symbolic", "Arm again")
                .connect_clicked(move |_| change(&window, id, |alerts, at| alerts[at].rearm()));
        }
        {
            let (window, alert) = (window.clone(), (*alert).clone());
            button("document-edit-symbolic", "Edit")
                .connect_clicked(move |_| alert_editor::present(&window, alert.clone()));
        }
        {
            let window = window.clone();
            button("edit-copy-symbolic", "Clone").connect_clicked(move |_| {
                // Into the editor as a new alert: a copy is made to be
                // changed, and one cancelled is one not made.
                let alerts = window.store().alerts();
                let Some(source) = alerts.iter().find(|a| a.id == id) else { return };
                alert_editor::present(&window, source.duplicate(chrono::Utc::now().timestamp()));
            });
        }
        {
            let window = window.clone();
            button("user-trash-symbolic", "Remove").connect_clicked(move |_| {
                change(&window, id, |alerts, at| {
                    alerts.remove(at);
                })
            });
        }
        grid.attach(&actions, 9, row, 1, 1);
    }
    grid.upcast()
}

/// Every firing, newest first.
fn log_table(window: &Rc<Window>, alerts: &[Alert]) -> gtk::Widget {
    let hits = crate::alerts::hits(alerts);
    if hits.is_empty() {
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("document-open-recent-symbolic"));
        empty.set_title("Nothing has fired");
        empty.set_description(Some("Each time an alert fires, it is written down here with the price."));
        return empty.upcast();
    }
    let grid = gtk::Grid::new();
    grid.set_column_spacing(18);
    grid.set_row_spacing(6);
    grid.set_margin_top(12);
    grid.set_margin_bottom(18);
    grid.set_margin_start(18);
    grid.set_margin_end(18);
    for (column, title) in ["Time", "Symbol", "Alert", "Level", "Price"].iter().enumerate() {
        let label = gtk::Label::new(Some(title));
        label.add_css_class("dim-label");
        label.add_css_class("caption-heading");
        label.set_xalign(if column >= 3 { 1.0 } else { 0.0 });
        grid.attach(&label, column as i32, 0, 1, 1);
    }
    grid.attach(&gtk::Separator::new(gtk::Orientation::Horizontal), 0, 1, 5, 1);
    let index = window.symbols();
    for (n, (alert, hit)) in hits.iter().enumerate() {
        let kind = index.as_ref().and_then(|i| i.find(&alert.symbol, alert.suffix.as_deref())).map(|i| i.kind);
        let decimals = price_decimals(0.0, alert.scale(), kind);
        let name = match alert.name.is_empty() {
            true => format!("#{} {}", alert.id, alert.spell_condition()),
            false => format!("#{} {} · {}", alert.id, alert.name, alert.spell_condition()),
        };
        let cells = [
            when(hit.at),
            alert.spell_symbol(),
            name,
            alert.spell_level(decimals),
            match hit.price.is_finite() {
                true => grouped_decimals(hit.price, decimals),
                false => "—".into(),
            },
        ];
        for (column, text) in cells.iter().enumerate() {
            let label = gtk::Label::new(Some(text));
            label.set_xalign(if column >= 3 { 1.0 } else { 0.0 });
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            if column >= 3 {
                label.add_css_class("numeric");
            }
            if column == 2 {
                label.set_hexpand(true);
            }
            grid.attach(&label, column as i32, n as i32 + 2, 1, 1);
        }
    }
    grid.upcast()
}

/// Where an alert's level is now: its line's price, for one on a line.
fn level_now(window: &Window, alert: &Alert) -> f64 {
    alert
        .drawing
        .and_then(|id| window.store().drawing_by_id(id))
        .map_or(alert.level, |(_, _, line)| line.from.price)
}

/// How far the price has to go, in percent: to the level, or to the nearer
/// bound of a channel — zero when it is inside one.
fn distance(alert: &Alert, level: f64, last: Option<f64>) -> Option<f64> {
    let last = last.filter(|l| *l != 0.0)?;
    let target = match alert.condition.is_channel() {
        true => {
            let (lo, hi) = alert.bounds();
            if (lo..=hi).contains(&last) {
                return Some(0.0);
            }
            if (lo - last).abs() < (hi - last).abs() { lo } else { hi }
        }
        false => level,
    };
    Some((target - last) / last * 100.0)
}

/// `14:32` today, `Oct 30 14:32` otherwise.
fn when(at: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(moment) = Local.timestamp_opt(at, 0).single() else { return String::new() };
    match moment.date_naive() == Local::now().date_naive() {
        true => moment.format("%H:%M").to_string(),
        false => moment.format("%b %-d %H:%M").to_string(),
    }
}
