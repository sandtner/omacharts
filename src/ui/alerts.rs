//! The alerts, as a table: what each is on, where its level is, how far the
//! price is from it, and whether it has fired.
//!
//! A grid rather than a list of rows, because the point of looking is to
//! compare — which is nearest, which have gone off — and that is a glance down
//! a column. Drawn again whole after any change made here; a change made from
//! a terminal while it is open shows the next time it is opened.

use std::cell::RefCell;
use std::rc::Rc;

/// The way back to redrawing the table, for a button in it.
type Rebuild = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

fn redraw(slot: &Rebuild) {
    let again = slot.borrow().clone();
    if let Some(again) = again {
        again();
    }
}

use adw::prelude::*;
use omacharts_engine::alert::Alert;
use omacharts_engine::{grouped_decimals, price_decimals};

use crate::ui::Window;

/// Open the table.
pub fn present(window: &Rc<Window>) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Alerts");
    dialog.set_content_width(760);
    dialog.set_content_height(420);

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&holder));

    let header = adw::HeaderBar::new();
    let clear = gtk::Button::with_label("Clear fired");
    clear.set_tooltip_text(Some("Remove every alert that has fired"));
    header.pack_start(&clear);
    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&scroller));
    dialog.set_child(Some(&root));

    // The table's buttons redraw the table, so the redraw is reached through
    // a slot rather than captured: emptied when the dialog closes, which is
    // what lets the widgets and the closure go rather than hold each other.
    let slot: Rebuild = Rc::new(RefCell::new(None));
    let rebuild: Rc<dyn Fn()> = {
        let window = window.clone();
        let holder = holder.clone();
        let dialog = dialog.clone();
        let clear = clear.clone();
        let slot = slot.clone();
        Rc::new(move || {
            while let Some(child) = holder.first_child() {
                holder.remove(&child);
            }
            let alerts = window.store().alerts();
            clear.set_sensitive(alerts.iter().any(|a| !a.is_armed()));
            holder.append(&table(&window, &dialog, &alerts, slot.clone()));
        })
    };
    *slot.borrow_mut() = Some(rebuild.clone());
    {
        let slot = slot.clone();
        dialog.connect_closed(move |_| {
            slot.borrow_mut().take();
        });
    }
    {
        let window = window.clone();
        let rebuild = rebuild.clone();
        clear.connect_clicked(move |_| {
            let mut alerts = window.store().alerts();
            alerts.retain(Alert::is_armed);
            window.store().set_alerts(&alerts);
            window.refresh_alert_levels();
            rebuild();
        });
    }
    rebuild();
    dialog.present(Some(&window.window));
}

const COLUMNS: [&str; 8] = ["Symbol", "Condition", "Level", "Last", "Distance", "Note", "Status", ""];

fn table(
    window: &Rc<Window>,
    dialog: &adw::Dialog,
    alerts: &[Alert],
    rebuild: Rebuild,
) -> gtk::Widget {
    if alerts.is_empty() {
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("alarm-symbolic"));
        empty.set_title("No alerts");
        empty.set_description(Some(
            "Right-click a horizontal line and choose “Alert when crossed”, or run\nomacharts alert add AAPL --above 250",
        ));
        return empty.upcast();
    }

    let grid = gtk::Grid::new();
    grid.set_column_spacing(18);
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

    // Armed first, nearest first; then what has fired, latest first. The
    // question the table is opened to answer is "what is about to go off".
    let index = window.symbols();
    let provider = window.provider();
    let mut rows: Vec<(&Alert, Option<f64>, usize)> = alerts
        .iter()
        .map(|alert| {
            let instrument = index.as_ref().and_then(|i| i.find(&alert.symbol, alert.suffix.as_deref()));
            let level = level_now(window, alert);
            let last = instrument
                .and_then(|i| crate::cli::quote(window.store(), provider.as_ref(), i))
                .map(|q| q.last)
                .or(alert.last);
            let decimals = price_decimals(0.0, level, instrument.map(|i| i.kind));
            (alert, last, decimals)
        })
        .collect();
    let distance = |alert: &Alert, last: Option<f64>| {
        last.filter(|l| *l != 0.0).map(|l| (level_now(window, alert) - l) / l * 100.0)
    };
    rows.sort_by(|(a, la, _), (b, lb, _)| {
        a.fired.is_some().cmp(&b.fired.is_some()).then_with(|| match (a.fired, b.fired) {
            (Some(x), Some(y)) => y.cmp(&x),
            _ => {
                let near = |alert, last| distance(alert, last).map_or(f64::MAX, f64::abs);
                near(a, *la).total_cmp(&near(b, *lb))
            }
        })
    });

    for (n, (alert, last, decimals)) in rows.into_iter().enumerate() {
        let row = n as i32 + 2;
        let level = level_now(window, alert);
        let price = |p: f64| grouped_decimals(p, decimals);

        // The symbol is the way to its chart.
        let symbol = gtk::Button::with_label(&alert.spell_symbol());
        symbol.add_css_class("flat");
        symbol.set_halign(gtk::Align::Start);
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
        grid.attach(&symbol, 0, row, 1, 1);

        let condition = match alert.drawing {
            Some(id) => format!("{} · line #{id}", alert.condition.key()),
            None => alert.condition.key().to_string(),
        };
        let away = distance(alert, last).map(|d| format!("{d:+.2}%")).unwrap_or_else(|| "—".into());
        let status = match alert.fired {
            None => "Armed".to_string(),
            Some(at) => format!("Fired {}", when(at)),
        };
        let cells = [
            condition,
            price(level),
            last.map(price).unwrap_or_else(|| "—".into()),
            away,
            alert.note.clone(),
            status,
        ];
        for (column, text) in cells.iter().enumerate() {
            let column = column + 1;
            let label = gtk::Label::new(Some(text));
            label.set_xalign(if numeric(column) { 1.0 } else { 0.0 });
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            if numeric(column) {
                label.add_css_class("numeric");
            }
            if column == 5 {
                label.set_max_width_chars(18);
                label.set_hexpand(true);
            }
            if column == 6 && alert.fired.is_some() {
                label.add_css_class("warning");
            }
            grid.attach(&label, column as i32, row, 1, 1);
        }

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        if !alert.is_armed() {
            let rearm = gtk::Button::from_icon_name("view-refresh-symbolic");
            rearm.add_css_class("flat");
            rearm.set_tooltip_text(Some("Arm again"));
            let (window, rebuild, id) = (window.clone(), rebuild.clone(), alert.id);
            rearm.connect_clicked(move |_| {
                let mut alerts = window.store().alerts();
                if let Some(a) = alerts.iter_mut().find(|a| a.id == id) {
                    a.rearm();
                }
                window.store().set_alerts(&alerts);
                window.refresh_alert_levels();
                redraw(&rebuild);
            });
            actions.append(&rearm);
        }
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("Remove"));
        let (window, rebuild, id) = (window.clone(), rebuild.clone(), alert.id);
        remove.connect_clicked(move |_| {
            let mut alerts = window.store().alerts();
            alerts.retain(|a| a.id != id);
            window.store().set_alerts(&alerts);
            window.refresh_alert_levels();
            redraw(&rebuild);
        });
        actions.append(&remove);
        actions.set_halign(gtk::Align::End);
        grid.attach(&actions, 7, row, 1, 1);
    }
    grid.upcast()
}

/// Level, Last and Distance read as figures, right-aligned.
fn numeric(column: usize) -> bool {
    matches!(column, 2..=4)
}

/// Where an alert's level is now: its line's price, for one on a line.
fn level_now(window: &Window, alert: &Alert) -> f64 {
    alert
        .drawing
        .and_then(|id| window.store().drawing_by_id(id))
        .map_or(alert.level, |(_, _, line)| line.from.price)
}

/// `14:32` today, `Oct 30 14:32` before.
fn when(at: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(moment) = Local.timestamp_opt(at, 0).single() else { return String::new() };
    match moment.date_naive() == Local::now().date_naive() {
        true => moment.format("%H:%M").to_string(),
        false => moment.format("%b %-d %H:%M").to_string(),
    }
}
