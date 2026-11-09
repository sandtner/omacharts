//! The backtest dock: a strip under the charts where a strategy is written,
//! run over the focused chart, and judged.
//!
//! Under the charts rather than over them, because the chart is half the
//! answer: the trades are laid over its bars (see the chart's
//! `draw_backtest_*`), and a dialog would cover them. It reads the focused
//! chart — its symbol, its resolution, the very bars it draws — so what
//! the dock reports is what the chart shows, and it runs again as the form
//! changes, a moment after the last keystroke, and when the focus, the
//! symbol or the resolution moves.
//!
//! Three parts, left to right: the form; the verdict, as figures and an
//! equity curve set against simply holding; and the trades, each of which
//! brings itself into view on the chart when picked.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use omacharts_engine::backtest::{Reason, Report};
use omacharts_engine::condition::Inputs;
use omacharts_engine::Bar;

use crate::backtests::{self, Field, Request};
use crate::ui::colors;
use crate::ui::window::Window;

/// How long after the last keystroke the dock runs again. Long enough not
/// to run on every letter of a rule, short enough to feel immediate.
const DEBOUNCE_MS: u64 = 250;

/// The period presets, as the dropdown shows them, in [`backtests::PERIODS`]' order.
const PERIOD_LABELS: [&str; 8] = ["1 month", "3 months", "6 months", "1 year", "2 years", "5 years", "All", "Custom"];

/// What the equity curve draws: the account at each bar's close, and the
/// same account simply holding the symbol over the same bars.
#[derive(Default)]
struct Curve {
    equity: Vec<f64>,
    holding: Vec<f64>,
    account: f64,
    first_ts: i64,
    last_ts: i64,
    intraday: bool,
}

/// One figure in the verdict: what it is, the figure, and a line under it.
struct Tile {
    value: gtk::Label,
    detail: gtk::Label,
}

pub struct Dock {
    pub root: gtk::Box,
    window: Weak<Window>,
    subject: gtk::Label,
    entry: gtk::Entry,
    exit: gtk::Entry,
    stop: gtk::Entry,
    target: gtk::Entry,
    account: gtk::Entry,
    risk: gtk::Entry,
    commission: gtk::Entry,
    slippage: gtk::Entry,
    period: gtk::DropDown,
    from: gtk::Entry,
    to: gtk::Entry,
    problem: gtk::Label,
    tiles: Vec<Tile>,
    equity: gtk::DrawingArea,
    curve: Rc<RefCell<Curve>>,
    trades: gtk::ListBox,
    trades_title: gtk::Label,
    /// What the last run found, and on which chart, for picking a trade.
    last: RefCell<Option<(u32, crate::backtests::Marks, Vec<omacharts_engine::backtest::Trade>)>>,
    /// Bumped by each change, so only the last of a burst runs.
    generation: Cell<u64>,
    /// Set while the form is being filled from a stored request, so filling
    /// it does not count as somebody editing it.
    loading: Cell<bool>,
    /// The saved strategies, as the header's list offers them: "Unsaved"
    /// first, then each by name, their ids in the same order after it.
    strategies: gtk::DropDown,
    strategy_ids: RefCell<Vec<i64>>,
    /// The saved strategy the form was filled from, while it is chosen.
    chosen: Cell<Option<i64>>,
    /// "edited": the form no longer says what the chosen strategy does.
    edited_mark: gtk::Label,
    save: gtk::Button,
    /// The name asked for when saving as new or renaming.
    naming: gtk::Popover,
    name_entry: gtk::Entry,
    name_problem: gtk::Label,
    /// Whether the name popover renames the chosen strategy rather than
    /// saving a new one.
    renaming: Cell<bool>,
}

fn field(placeholder: &str, width: i32) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_width_chars(width);
    entry.set_hexpand(width > 12);
    entry
}

fn caption(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    label.add_css_class("caption-heading");
    label
}

impl Dock {
    pub fn new(window: &Rc<Window>) -> Rc<Dock> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("backtest-dock");
        root.add_css_class("view");

        // -- Header: what is tested, over when ----------------------------
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_start(12);
        header.set_margin_end(8);
        header.set_margin_top(6);
        header.set_margin_bottom(4);
        let title = gtk::Label::new(Some("Backtest"));
        title.add_css_class("heading");
        header.append(&title);
        // The saved strategies: pick one to fill the form, save the form as
        // one, and the rest from the menu beside it.
        let strategies = gtk::DropDown::from_strings(&["Unsaved strategy"]);
        strategies.set_tooltip_text(Some("A saved strategy fills the rules; the period and the money stay"));
        header.append(&strategies);
        let edited_mark = gtk::Label::new(Some("edited"));
        edited_mark.add_css_class("dim-label");
        edited_mark.add_css_class("caption");
        edited_mark.set_visible(false);
        header.append(&edited_mark);
        let save = gtk::Button::with_label("Save");
        save.set_tooltip_text(Some("Save the rules as the chosen strategy, or as a new one"));
        header.append(&save);
        let menu = gtk::gio::Menu::new();
        menu.append(Some("Save as new…"), Some("dock.save-as"));
        menu.append(Some("Rename…"), Some("dock.rename"));
        menu.append(Some("Delete"), Some("dock.delete"));
        let more = gtk::MenuButton::new();
        more.set_icon_name("view-more-symbolic");
        more.add_css_class("flat");
        more.set_menu_model(Some(&menu));
        more.set_tooltip_text(Some("More about saved strategies"));
        header.append(&more);
        let naming = gtk::Popover::new();
        let name_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        name_box.set_margin_start(6);
        name_box.set_margin_end(6);
        name_box.set_margin_top(6);
        name_box.set_margin_bottom(6);
        let name_entry = gtk::Entry::new();
        name_entry.set_placeholder_text(Some("Name, such as Trend SPY"));
        name_entry.set_width_chars(24);
        let name_problem = gtk::Label::new(None);
        name_problem.add_css_class("error");
        name_problem.add_css_class("caption");
        name_problem.set_xalign(0.0);
        name_problem.set_visible(false);
        let name_ok = gtk::Button::with_label("Save");
        name_ok.add_css_class("suggested-action");
        name_box.append(&name_entry);
        name_box.append(&name_problem);
        name_box.append(&name_ok);
        naming.set_child(Some(&name_box));
        naming.set_parent(&save);
        let subject = gtk::Label::new(None);
        subject.add_css_class("dim-label");
        subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subject.set_hexpand(true);
        subject.set_xalign(0.0);
        header.append(&subject);
        let period = gtk::DropDown::from_strings(&PERIOD_LABELS);
        period.set_tooltip_text(Some("The bars traded on, back from the last closed bar"));
        let from = field("from 2026-01-01", 11);
        let to = field("to (last bar)", 11);
        header.append(&period);
        header.append(&from);
        header.append(&to);
        let fit = gtk::Button::from_icon_name("zoom-fit-best-symbolic");
        fit.add_css_class("flat");
        fit.set_tooltip_text(Some("Show the tested period on the chart"));
        header.append(&fit);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Close the backtest (Ctrl+Shift+T)"));
        header.append(&close);
        root.append(&header);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        body.set_margin_start(12);
        body.set_margin_end(12);
        body.set_margin_top(8);
        body.set_margin_bottom(8);
        body.set_vexpand(true);

        // -- The form -------------------------------------------------------
        let form = gtk::Grid::new();
        form.set_row_spacing(6);
        form.set_column_spacing(8);
        form.set_size_request(300, -1);
        let entry = field("sma(20) crosses_above sma(50)", 16);
        let exit = field("sma(20) crosses_below sma(50)", 16);
        let stop = field("2*atr(14) or 3%", 7);
        let target = field("2R or 8%", 7);
        let account = field("config risk", 7);
        let risk = field("1%", 7);
        let commission = field("0", 7);
        let slippage = field("0.05", 7);
        let row = |at: i32, name: &str, widget: &gtk::Entry, span: i32| {
            form.attach(&caption(name), 0, at, 1, 1);
            form.attach(widget, 1, at, span, 1);
        };
        row(0, "Buy when", &entry, 3);
        row(1, "Sell when", &exit, 3);
        row(2, "Stop", &stop, 1);
        form.attach(&caption("Target"), 2, 2, 1, 1);
        form.attach(&target, 3, 2, 1, 1);
        row(3, "Account", &account, 1);
        form.attach(&caption("Risk"), 2, 3, 1, 1);
        form.attach(&risk, 3, 3, 1, 1);
        row(4, "Commission", &commission, 1);
        form.attach(&caption("Slippage %"), 2, 4, 1, 1);
        form.attach(&slippage, 3, 4, 1, 1);
        let problem = gtk::Label::new(None);
        problem.set_xalign(0.0);
        problem.set_wrap(true);
        problem.set_max_width_chars(38);
        problem.add_css_class("error");
        problem.add_css_class("caption");
        form.attach(&problem, 0, 5, 4, 1);
        let hint = gtk::Label::new(Some(
            "Long only. Decides on closed bars, fills at the next open. A stop sizes each trade by the risk.",
        ));
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        hint.set_max_width_chars(38);
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        form.attach(&hint, 0, 6, 4, 1);
        body.append(&form);

        // -- The verdict: figures over the equity curve -----------------------
        let middle = gtk::Box::new(gtk::Orientation::Vertical, 8);
        middle.set_hexpand(true);
        // A flow rather than a row: seven figures side by side on a wide
        // window, two rows of them on a narrow one, never cut off.
        let row_of_tiles = gtk::FlowBox::new();
        row_of_tiles.set_homogeneous(true);
        row_of_tiles.set_selection_mode(gtk::SelectionMode::None);
        row_of_tiles.set_min_children_per_line(3);
        row_of_tiles.set_max_children_per_line(7);
        row_of_tiles.set_column_spacing(6);
        row_of_tiles.set_row_spacing(6);
        let mut tiles = Vec::new();
        for name in ["Net result", "Buy and hold", "Trades", "Profit factor", "Average R", "Max drawdown", "In the market"] {
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
            tile.add_css_class("card");
            let label = |text: &str, classes: &[&str]| {
                let l = gtk::Label::new(Some(text));
                l.set_xalign(0.0);
                l.set_margin_start(8);
                l.set_margin_end(8);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                for class in classes {
                    l.add_css_class(class);
                }
                l
            };
            let name = label(name, &["dim-label", "caption-heading"]);
            name.set_margin_top(5);
            let value = label("—", &["title-4", "numeric"]);
            let detail = label("", &["dim-label", "caption", "numeric"]);
            detail.set_margin_bottom(5);
            tile.append(&name);
            tile.append(&value);
            tile.append(&detail);
            row_of_tiles.insert(&tile, -1);
            tiles.push(Tile { value, detail });
        }
        middle.append(&row_of_tiles);
        let equity = gtk::DrawingArea::new();
        equity.set_vexpand(true);
        equity.set_hexpand(true);
        equity.set_content_height(110);
        middle.append(&equity);
        body.append(&middle);

        // -- The trades -------------------------------------------------------
        let right = gtk::Box::new(gtk::Orientation::Vertical, 4);
        right.set_size_request(330, -1);
        let trades_title = caption("Trades");
        right.append(&trades_title);
        let trades = gtk::ListBox::new();
        trades.add_css_class("boxed-list");
        trades.set_selection_mode(gtk::SelectionMode::Single);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&trades));
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        right.append(&scroller);
        body.append(&right);
        // Scrolled sideways rather than pushing the window wider, should a
        // narrow window still not hold all three parts.
        let room = gtk::ScrolledWindow::new();
        room.set_vscrollbar_policy(gtk::PolicyType::Never);
        room.set_hscrollbar_policy(gtk::PolicyType::Automatic);
        room.set_propagate_natural_height(true);
        room.set_vexpand(true);
        room.set_child(Some(&body));
        root.append(&room);

        let curve = Rc::new(RefCell::new(Curve::default()));
        let dock = Rc::new(Dock {
            root,
            window: Rc::downgrade(window),
            subject,
            entry,
            exit,
            stop,
            target,
            account,
            risk,
            commission,
            slippage,
            period,
            from,
            to,
            problem,
            tiles,
            equity,
            curve,
            trades,
            trades_title,
            last: RefCell::new(None),
            generation: Cell::new(0),
            loading: Cell::new(false),
            strategies,
            strategy_ids: RefCell::new(Vec::new()),
            chosen: Cell::new(None),
            edited_mark,
            save,
            naming,
            name_entry,
            name_problem,
            renaming: Cell::new(false),
        });
        {
            let weak = Rc::downgrade(&dock);
            dock.strategies.connect_selected_notify(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.strategy_picked();
                }
            });
        }
        {
            let weak = Rc::downgrade(&dock);
            dock.save.connect_clicked(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.save_clicked();
                }
            });
        }
        {
            let weak = Rc::downgrade(&dock);
            name_ok.connect_clicked(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.name_given();
                }
            });
        }
        {
            let weak = Rc::downgrade(&dock);
            dock.name_entry.connect_activate(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.name_given();
                }
            });
        }
        let actions = gtk::gio::SimpleActionGroup::new();
        for (name, run) in [
            ("save-as", Dock::ask_name_new as fn(&Rc<Dock>)),
            ("rename", Dock::ask_name_rename as fn(&Rc<Dock>)),
            ("delete", Dock::delete_chosen as fn(&Rc<Dock>)),
        ] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(&dock);
            action.connect_activate(move |_, _| {
                if let Some(dock) = weak.upgrade() {
                    run(&dock);
                }
            });
            actions.add_action(&action);
        }
        dock.root.insert_action_group("dock", Some(&actions));
        dock.reload_strategies();

        {
            let curve = dock.curve.clone();
            let window = Rc::downgrade(window);
            dock.equity.set_draw_func(move |area, cr, w, h| {
                let Some(window) = window.upgrade() else { return };
                draw_curve(area, cr, w as f64, h as f64, &curve.borrow(), &window);
            });
        }
        for entry in dock.entries() {
            let weak = Rc::downgrade(&dock);
            entry.connect_changed(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.edited();
                }
            });
        }
        {
            let weak = Rc::downgrade(&dock);
            dock.period.connect_selected_notify(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.sync_period();
                    dock.edited();
                }
            });
        }
        {
            let weak = Rc::downgrade(&dock);
            dock.trades.connect_row_activated(move |_, row| {
                if let Some(dock) = weak.upgrade() {
                    dock.pick(row.index());
                }
            });
        }
        {
            let weak = Rc::downgrade(&dock);
            fit.connect_clicked(move |_| {
                if let Some(dock) = weak.upgrade() {
                    dock.fit();
                }
            });
        }
        {
            let window = Rc::downgrade(window);
            close.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.toggle_backtest();
                }
            });
        }
        dock.load(&Request::from_json(window.store().setting(backtests::SETTING).as_deref()));
        dock
    }

    fn entries(&self) -> [&gtk::Entry; 10] {
        [
            &self.entry,
            &self.exit,
            &self.stop,
            &self.target,
            &self.account,
            &self.risk,
            &self.commission,
            &self.slippage,
            &self.from,
            &self.to,
        ]
    }

    /// Fill the form from a request, as the last session left it or as a
    /// command asked for.
    pub fn load(&self, request: &Request) {
        self.loading.set(true);
        for (entry, text) in self.entries().into_iter().zip([
            &request.entry,
            &request.exit,
            &request.stop,
            &request.target,
            &request.account,
            &request.risk,
            &request.commission,
            &request.slippage,
            &request.from,
            &request.to,
        ]) {
            if entry.text() != text.as_str() {
                entry.set_text(text);
            }
        }
        let at = backtests::PERIODS.iter().position(|p| *p == request.period).unwrap_or(6);
        self.period.set_selected(at as u32);
        self.sync_period();
        let known = request.strategy.filter(|id| self.strategy_ids.borrow().contains(id));
        self.chosen.set(known);
        self.select_chosen();
        self.loading.set(false);
        self.sync_saved();
    }

    /// Fill the list of saved strategies from the store, keeping the one
    /// chosen if it is still there.
    pub fn reload_strategies(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let list = window.store().strategies();
        let mut names = vec!["Unsaved strategy".to_string()];
        names.extend(list.iter().map(|s| s.name.clone()));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let was = self.loading.replace(true);
        self.strategies.set_model(Some(&gtk::StringList::new(&refs)));
        *self.strategy_ids.borrow_mut() = list.iter().map(|s| s.id).collect();
        if self.chosen.get().is_some_and(|id| !list.iter().any(|s| s.id == id)) {
            self.chosen.set(None);
        }
        self.select_chosen();
        self.loading.set(was);
        self.sync_saved();
    }

    fn select_chosen(&self) {
        let at = self
            .chosen
            .get()
            .and_then(|id| self.strategy_ids.borrow().iter().position(|x| *x == id))
            .map_or(0, |at| at + 1);
        self.strategies.set_selected(at as u32);
    }

    /// The saved strategy chosen, as it is stored now.
    fn chosen_saved(&self) -> Option<omacharts_engine::strategy::Saved> {
        let window = self.window.upgrade()?;
        let id = self.chosen.get()?;
        window.store().strategies().into_iter().find(|s| s.id == id)
    }

    /// Whether the form's rules are the chosen strategy's, as the rule
    /// reader spells both.
    fn rules_match(&self, saved: &omacharts_engine::strategy::Saved) -> bool {
        let spelled = |entry: &str, exit: &str, stop: &str, target: &str| {
            omacharts_engine::strategy::parts(entry, exit, stop, target).ok().map(|s| {
                (
                    s.entry.to_string(),
                    s.exit.map(|r| r.to_string()),
                    s.stop.map(omacharts_engine::backtest::Stop::spell),
                    s.target.map(omacharts_engine::backtest::Target::spell),
                )
            })
        };
        let form = spelled(&self.entry.text(), &self.exit.text(), &self.stop.text(), &self.target.text());
        form.is_some() && form == spelled(&saved.entry, &saved.exit, &saved.stop, &saved.target)
    }

    /// The header says whether the form still is the chosen strategy.
    fn sync_saved(&self) {
        let saved = self.chosen_saved();
        let edited = saved.as_ref().is_some_and(|s| !self.rules_match(s));
        self.edited_mark.set_visible(edited);
        self.save.set_label(if saved.is_some() { "Save" } else { "Save…" });
        self.save.set_sensitive(saved.is_none() || edited);
    }

    /// A strategy picked in the header: its rules go into the form.
    fn strategy_picked(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        let at = self.strategies.selected() as usize;
        let id = at.checked_sub(1).and_then(|i| self.strategy_ids.borrow().get(i).copied());
        self.chosen.set(id);
        if let Some(saved) = self.chosen_saved() {
            self.loading.set(true);
            for (entry, text) in [
                (&self.entry, &saved.entry),
                (&self.exit, &saved.exit),
                (&self.stop, &saved.stop),
                (&self.target, &saved.target),
            ] {
                entry.set_text(text);
            }
            self.loading.set(false);
        }
        self.sync_saved();
        self.schedule();
    }

    /// Save: over the chosen strategy, or as a new one under a name.
    fn save_clicked(self: &Rc<Self>) {
        match self.chosen_saved() {
            Some(saved) => {
                let mut changed = saved.clone();
                changed.entry = self.entry.text().to_string();
                changed.exit = self.exit.text().to_string();
                changed.stop = self.stop.text().to_string();
                changed.target = self.target.text().to_string();
                if let Err(why) = self.store_strategy(changed, false) {
                    self.problem.set_text(&why);
                }
            }
            None => self.ask_name_new(),
        }
    }

    fn ask_name_new(self: &Rc<Self>) {
        self.renaming.set(false);
        self.name_entry.set_text("");
        self.name_problem.set_visible(false);
        self.naming.popup();
        self.name_entry.grab_focus();
    }

    fn ask_name_rename(self: &Rc<Self>) {
        let Some(saved) = self.chosen_saved() else {
            self.problem.set_text("Choose a saved strategy to rename");
            return;
        };
        self.renaming.set(true);
        self.name_entry.set_text(&saved.name);
        self.name_problem.set_visible(false);
        self.naming.popup();
        self.name_entry.grab_focus();
    }

    /// The name popover's Save: a new strategy from the form, or the chosen
    /// one renamed.
    fn name_given(self: &Rc<Self>) {
        let Some(window) = self.window.upgrade() else { return };
        let name = self.name_entry.text().to_string();
        let saved = match self.renaming.get() {
            true => match self.chosen_saved() {
                Some(mut saved) => {
                    saved.name = name;
                    saved
                }
                None => return,
            },
            false => {
                let list = window.store().strategies();
                omacharts_engine::strategy::Saved {
                    id: omacharts_engine::strategy::next_id(&list),
                    name,
                    entry: self.entry.text().to_string(),
                    exit: self.exit.text().to_string(),
                    stop: self.stop.text().to_string(),
                    target: self.target.text().to_string(),
                    timeframe: Some(window.focused_pane().timeframe.get()),
                    note: String::new(),
                    created: chrono::Utc::now().timestamp(),
                }
            }
        };
        if let Err(why) = self.store_strategy(saved, true) {
            self.name_problem.set_text(&why);
            self.name_problem.set_visible(true);
            return;
        }
        self.naming.popdown();
    }

    /// Write a strategy, new or changed, and choose it.
    fn store_strategy(self: &Rc<Self>, saved: omacharts_engine::strategy::Saved, named: bool) -> Result<(), String> {
        let Some(window) = self.window.upgrade() else { return Ok(()) };
        let saved = match saved.normalised() {
            Ok(saved) => saved,
            Err(why) => {
                if !named {
                    self.problem.set_text(&why);
                }
                return Err(why);
            }
        };
        let mut list = window.store().strategies();
        if omacharts_engine::strategy::taken(&list, &saved.name, Some(saved.id)) {
            return Err(format!("there is already a strategy called {:?}", saved.name));
        }
        match list.iter_mut().find(|s| s.id == saved.id) {
            Some(stored) => *stored = saved.clone(),
            None => list.push(saved.clone()),
        }
        window.store().set_strategies(&list);
        self.chosen.set(Some(saved.id));
        self.reload_strategies();
        self.schedule();
        Ok(())
    }

    /// Delete the chosen strategy, after asking: it cannot be had back.
    fn delete_chosen(self: &Rc<Self>) {
        let Some(saved) = self.chosen_saved() else {
            self.problem.set_text("Choose a saved strategy to delete");
            return;
        };
        let Some(window) = self.window.upgrade() else { return };
        let ask = adw::AlertDialog::new(
            Some(&format!("Delete {:?}?", saved.name)),
            Some("The rules stay in the form until they are changed; the saved strategy is gone."),
        );
        ask.add_response("cancel", "Cancel");
        ask.add_response("delete", "Delete");
        ask.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        ask.set_default_response(Some("cancel"));
        let weak = Rc::downgrade(self);
        let id = saved.id;
        ask.connect_response(None, move |_, response| {
            let Some(dock) = weak.upgrade() else { return };
            let Some(window) = dock.window.upgrade() else { return };
            if response != "delete" {
                return;
            }
            let mut list = window.store().strategies();
            list.retain(|s| s.id != id);
            window.store().set_strategies(&list);
            dock.chosen.set(None);
            dock.reload_strategies();
            dock.schedule();
        });
        ask.present(Some(&window.window));
    }

    /// What the form says now.
    pub fn request(&self) -> Request {
        Request {
            entry: self.entry.text().to_string(),
            exit: self.exit.text().to_string(),
            stop: self.stop.text().to_string(),
            target: self.target.text().to_string(),
            period: backtests::PERIODS.get(self.period.selected() as usize).unwrap_or(&"all").to_string(),
            from: self.from.text().to_string(),
            to: self.to.text().to_string(),
            account: self.account.text().to_string(),
            risk: self.risk.text().to_string(),
            commission: self.commission.text().to_string(),
            slippage: self.slippage.text().to_string(),
            strategy: self.chosen.get(),
        }
    }

    fn sync_period(&self) {
        let custom = backtests::PERIODS.get(self.period.selected() as usize) == Some(&"custom");
        self.from.set_visible(custom);
        self.to.set_visible(custom);
    }

    /// Something in the form changed: run again once the typing stops.
    fn edited(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        self.sync_saved();
        self.schedule();
    }

    /// Run again shortly; a burst of reasons runs once.
    pub fn schedule(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(DEBOUNCE_MS), move || {
            if let Some(dock) = weak.upgrade()
                && dock.generation.get() == generation
                && dock.root.is_visible()
            {
                dock.run();
            }
        });
    }

    /// Run the form over the focused chart's bars and show what it found.
    pub fn run(self: &Rc<Self>) {
        let Some(window) = self.window.upgrade() else { return };
        let request = self.request();
        window.store().set_setting(backtests::SETTING, &request.to_json());
        for entry in self.entries() {
            entry.remove_css_class("error");
        }
        let pane = window.focused_pane();
        let instrument = pane.instrument.borrow().clone();
        let Some(instrument) = instrument else {
            self.fail(&window, None, "The focused chart shows no symbol");
            return;
        };
        let timeframe = pane.timeframe.get();
        let now = chrono::Utc::now().timestamp();
        let mut bars: Vec<Bar> = pane.view.with_bars(|b| b.to_vec());
        // Decided on closed bars only.
        if crate::alerts::forming(&instrument, &bars, timeframe, now) {
            bars.pop();
        }
        let display = match &instrument.suffix {
            Some(suffix) => format!("{}.{suffix}", instrument.symbol),
            None => instrument.symbol.clone(),
        };
        if bars.len() < 2 {
            self.fail(&window, None, &format!("{display} has no bars on this chart yet"));
            return;
        }
        let plan = match backtests::plan(&request, &window.store().position_risk()) {
            Ok(plan) => plan,
            Err((field, why)) => {
                self.fail(&window, Some(field), &why);
                return;
            }
        };
        let inputs = Inputs { session_origin: instrument.session_origin, timeframe };
        let report = match backtests::run(&plan, &request, &bars, &inputs) {
            Ok(report) => report,
            Err((field, why)) => {
                self.fail(&window, Some(field), &why);
                return;
            }
        };
        self.problem.set_text("");
        let marks = backtests::marks(&report, &bars);
        window.show_backtest_marks(pane.id, Some(marks.clone()));
        let day = |ts: i64| spell_day(ts, timeframe.is_intraday());
        self.subject.set_text(&format!(
            "{display} · {} · {} → {} · {} bars",
            timeframe.label(),
            day(bars[report.from].ts),
            day(bars[report.to - 1].ts),
            report.to - report.from
        ));
        self.show_figures(&report, &plan.risk);
        *self.curve.borrow_mut() = Curve {
            holding: bars[report.from..report.to]
                .iter()
                .map(|b| plan.settings.account * b.close / bars[report.from].close)
                .collect(),
            equity: report.equity.clone(),
            account: plan.settings.account,
            first_ts: bars[report.from].ts,
            last_ts: bars[report.to - 1].ts,
            intraday: timeframe.is_intraday(),
        };
        self.equity.queue_draw();
        self.show_trades(&report, &plan.risk, &instrument, timeframe.is_intraday());
        *self.last.borrow_mut() = Some((pane.id, marks, report.trades.clone()));
    }

    /// Say what is wrong, mark where, and take the trades off the chart: a
    /// result that no longer matches the form is worse than none.
    fn fail(&self, window: &Rc<Window>, field: Option<Field>, why: &str) {
        self.problem.set_text(why);
        if let Some(field) = field {
            let marked: Vec<&gtk::Entry> = match field {
                Field::Entry => vec![&self.entry],
                Field::Exit => vec![&self.exit],
                Field::Stop => vec![&self.stop],
                Field::Target => vec![&self.target],
                Field::Period => vec![&self.from, &self.to],
                Field::Account => vec![&self.account],
                Field::Risk => vec![&self.risk],
                Field::Costs => vec![&self.commission, &self.slippage],
            };
            for entry in marked {
                entry.add_css_class("error");
            }
        }
        window.show_backtest_marks(0, None);
        *self.last.borrow_mut() = None;
    }

    fn show_figures(&self, report: &Report, risk: &omacharts_engine::position::Risk) {
        let st = &report.stats;
        let signed = |v: f64| match v >= 0.0 {
            true => format!("+{}", risk.money(v)),
            false => format!("−{}", risk.money(-v)),
        };
        let pct = |v: f64| format!("{v:+.1}%");
        let values: [(String, String, Option<bool>); 7] = [
            (pct(st.return_pct), signed(st.net_profit), Some(st.net_profit >= 0.0)),
            (pct(st.buy_hold_pct), "over the same bars".into(), None),
            (
                st.trades.to_string(),
                match st.trades {
                    0 => "no closed trades".into(),
                    _ => format!("{:.0}% won", st.win_rate),
                },
                None,
            ),
            (st.profit_factor.map_or("—".into(), |p| format!("{p:.2}")), "won ÷ lost".into(), st.profit_factor.map(|p| p >= 1.0)),
            (
                st.average_r.map_or("—".into(), |r| format!("{r:+.2}R")),
                format!("{} a trade", signed(st.expectancy)),
                st.average_r.map(|r| r >= 0.0),
            ),
            (pct(st.max_drawdown_pct), "from a high".into(), None),
            (format!("{:.0}%", st.exposure_pct), format!("{:.0} bars a trade", st.average_bars), None),
        ];
        for (tile, (value, detail, good)) in self.tiles.iter().zip(values) {
            tile.value.set_text(&value);
            tile.detail.set_text(&detail);
            tile.value.remove_css_class("success");
            tile.value.remove_css_class("error");
            match good {
                Some(true) => tile.value.add_css_class("success"),
                Some(false) => tile.value.add_css_class("error"),
                None => {}
            }
        }
    }

    fn show_trades(
        &self,
        report: &Report,
        risk: &omacharts_engine::position::Risk,
        instrument: &omacharts_engine::Instrument,
        intraday: bool,
    ) {
        while let Some(row) = self.trades.row_at_index(0) {
            self.trades.remove(&row);
        }
        let decimals = report
            .trades
            .first()
            .map_or(2, |t| omacharts_engine::price_decimals(0.0, t.entry, Some(instrument.kind)));
        self.trades_title.set_text(&format!("Trades · {} · newest first, pick one to see it", report.trades.len()));
        for t in report.trades.iter().rev() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_start(8);
            row.set_margin_end(8);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            let when = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let dates = gtk::Label::new(Some(&format!(
                "{} → {}",
                spell_day(t.entry_ts, intraday),
                if t.reason == Reason::Open { "open".into() } else { spell_day(t.exit_ts, intraday) }
            )));
            dates.set_xalign(0.0);
            dates.add_css_class("numeric");
            let prices = gtk::Label::new(Some(&format!(
                "{:.decimals$} → {:.decimals$} · {} · {}",
                t.entry,
                t.exit,
                t.reason.key(),
                omacharts_engine::position::spell_quantity(t.units)
            )));
            prices.set_xalign(0.0);
            prices.add_css_class("dim-label");
            prices.add_css_class("caption");
            prices.add_css_class("numeric");
            when.append(&dates);
            when.append(&prices);
            when.set_hexpand(true);
            row.append(&when);
            let result = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let made = gtk::Label::new(Some(&crate::backtests::label(t)));
            made.set_xalign(1.0);
            made.add_css_class("numeric");
            made.add_css_class("heading");
            made.add_css_class(if t.pnl > 0.0 { "success" } else { "error" });
            let money = gtk::Label::new(Some(&match t.pnl >= 0.0 {
                true => format!("+{}", risk.money(t.pnl)),
                false => format!("−{}", risk.money(-t.pnl)),
            }));
            money.set_xalign(1.0);
            money.add_css_class("dim-label");
            money.add_css_class("caption");
            money.add_css_class("numeric");
            result.append(&made);
            result.append(&money);
            row.append(&result);
            self.trades.append(&row);
        }
    }

    /// A trade picked in the list: drawn stronger, and brought into view.
    fn pick(&self, row: i32) {
        let Some(window) = self.window.upgrade() else { return };
        let mut last = self.last.borrow_mut();
        let Some((pane, marks, trades)) = last.as_mut() else { return };
        // The list is newest first.
        let Some(at) = trades.len().checked_sub(1 + row.max(0) as usize) else { return };
        marks.selected = Some(at);
        window.show_backtest_marks(*pane, Some(marks.clone()));
        if let Some(view) = window.chart_view(*pane) {
            view.reveal(trades[at].entry_ts, trades[at].exit_ts);
        }
    }

    /// The whole tested period, on the chart.
    fn fit(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let last = self.last.borrow();
        let Some((pane, marks, _)) = last.as_ref() else { return };
        if let Some(view) = window.chart_view(*pane) {
            view.reveal(marks.from_ts, marks.to_ts);
        }
    }

    /// The dock is going away: take the trades off the chart.
    pub fn hidden(&self) {
        if let Some(window) = self.window.upgrade() {
            window.show_backtest_marks(0, None);
        }
        *self.last.borrow_mut() = None;
    }
}

fn spell_day(ts: i64, intraday: bool) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| {
            let local = t.with_timezone(&chrono::Local);
            match intraday {
                true => local.format("%Y-%m-%d %H:%M").to_string(),
                false => local.format("%Y-%m-%d").to_string(),
            }
        })
        .unwrap_or_default()
}

/// The equity curve: the strategy's account as a line over a soft fill,
/// green above where it started and red below, holding the symbol as a
/// thin dashed line on the same scale for comparison, and where it started
/// as a faint rule. The extremes and the ends are written at the edges.
fn draw_curve(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: f64, h: f64, curve: &Curve, window: &Window) {
    let theme = window.theme();
    let scheme = window.bar_scheme();
    let ui = &theme.ui;
    let n = curve.equity.len();
    if n < 2 {
        cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(11.0);
        colors::set_source(cr, &ui.text_muted);
        cr.move_to(8.0, h / 2.0);
        let _ = cr.show_text("The equity curve appears once the strategy runs.");
        return;
    }
    let _ = area;
    let (pad_l, pad_r, pad_t, pad_b) = (56.0, 8.0, 8.0, 18.0);
    let (pw, ph) = ((w - pad_l - pad_r).max(10.0), (h - pad_t - pad_b).max(10.0));
    let all = curve.equity.iter().chain(curve.holding.iter()).chain(std::iter::once(&curve.account));
    let (mut lo, mut hi) = all.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if hi - lo < 1e-9 {
        hi += 1.0;
        lo -= 1.0;
    }
    let span = hi - lo;
    lo -= span * 0.05;
    hi += span * 0.05;
    let x = |i: usize| pad_l + i as f64 / (n - 1) as f64 * pw;
    let y = |v: f64| pad_t + (hi - v) / (hi - lo) * ph;
    let base = y(curve.account);

    // Where it started.
    colors::set_source_alpha(cr, &ui.text_muted, 0.5);
    cr.set_line_width(1.0);
    cr.set_dash(&[2.0, 3.0], 0.0);
    cr.move_to(pad_l, base.round() + 0.5);
    cr.line_to(pad_l + pw, base.round() + 0.5);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    // The fill: the area between the curve and where it started, green
    // where it is above and red below, each clipped to its own side.
    for (above, colour) in [(true, &scheme.up), (false, &scheme.down)] {
        cr.save().ok();
        match above {
            true => cr.rectangle(pad_l, pad_t, pw, base - pad_t),
            false => cr.rectangle(pad_l, base, pw, pad_t + ph - base),
        }
        cr.clip();
        cr.move_to(x(0), base);
        for (i, v) in curve.equity.iter().enumerate() {
            cr.line_to(x(i), y(*v));
        }
        cr.line_to(x(n - 1), base);
        cr.close_path();
        colors::set_source_alpha(cr, colour, 0.18);
        let _ = cr.fill();
        cr.restore().ok();
    }

    // Holding, for comparison.
    colors::set_source_alpha(cr, &ui.text_muted, 0.9);
    cr.set_line_width(1.0);
    cr.set_dash(&[4.0, 3.0], 0.0);
    for (i, v) in curve.holding.iter().enumerate() {
        match i {
            0 => cr.move_to(x(i), y(*v)),
            _ => cr.line_to(x(i), y(*v)),
        }
    }
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    // The strategy.
    colors::set_source(cr, &ui.text);
    cr.set_line_width(1.6);
    for (i, v) in curve.equity.iter().enumerate() {
        match i {
            0 => cr.move_to(x(i), y(*v)),
            _ => cr.line_to(x(i), y(*v)),
        }
    }
    let _ = cr.stroke();

    // The scale's ends, the dates, and a key.
    cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
    cr.set_font_size(10.0);
    colors::set_source(cr, &ui.text_muted);
    let figure = |v: f64| omacharts_engine::grouped_decimals(v, 0);
    for (v, at) in [(hi, pad_t + 9.0), (lo, pad_t + ph)] {
        let text = figure(v);
        let tw = cr.text_extents(&text).map(|e| e.x_advance()).unwrap_or(0.0);
        cr.move_to(pad_l - 6.0 - tw, at);
        let _ = cr.show_text(&text);
    }
    let first = spell_day(curve.first_ts, curve.intraday);
    let last = spell_day(curve.last_ts, curve.intraday);
    cr.move_to(pad_l, h - 4.0);
    let _ = cr.show_text(&first);
    let lw = cr.text_extents(&last).map(|e| e.x_advance()).unwrap_or(0.0);
    cr.move_to(pad_l + pw - lw, h - 4.0);
    let _ = cr.show_text(&last);
    // The key, in the top corner, where there is room for it beside the
    // scale's top figure.
    let key = "── strategy   ╌╌ holding";
    let kw = cr.text_extents(key).map(|e| e.x_advance()).unwrap_or(0.0);
    if pw > kw + 40.0 {
        cr.move_to(pad_l + pw - kw, pad_t + 9.0);
        let _ = cr.show_text(key);
    }
}
