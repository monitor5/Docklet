//! What happens between a window and the engine: the one place that owns
//! the models, answers what the user does and says what to draw.
//!
//! A [`Controller`] lives on the UI thread. Events of the window go to
//! [`Controller::on_event`], messages of the engine to
//! [`Controller::on_message`]; both end by sending the window whatever
//! changed, as `view::UiUpdate`s. The window keeps no state of its own
//! about lists: which rows show and which are selected is in the
//! `ListModel`s here and nowhere else.
//!
//! Two rules it keeps:
//!
//! - Nothing is held for a window that is not shown. Closed or minimised,
//!   the engine is told the window is gone: no poll, no event stream, no
//!   followed log, no sampling.
//! - Docker is only asked once a status poll made since the window was last
//!   shown said the engine runs. What was true before the window went away
//!   is not acted on: the engine may have gone to sleep, and a request
//!   would wake it.

use std::collections::HashMap;
use std::sync::Arc;

use docklet_core::EngineState;
use docklet_engine::{ContainerAction, ResourceKind};

use crate::app::{App, List, Message};
use crate::control::{EngineStatus, Transition};
use crate::list::ListModel;
use crate::logs::SharedLog;
use crate::policy::{DiskUsageTrigger, WindowState, loads_disk_usage, uses_docker_api};
use crate::present::{self, TABS, key};
use crate::pull::can_pull;
use crate::removal::{PruneTarget, RemovalPlan, prune_summary, removal_summary};
use crate::screen::MIRRORED_NETWORKING_WARNING;
use crate::streams::LogMessage;
use crate::usage::UsageModel;
use crate::view::{self, TrayCommand, UiEvent, UiUpdate};

/// Where the controller sends what to draw. Called on the UI thread.
pub trait Ui {
    fn send(&self, update: UiUpdate);
}

impl<F: Fn(UiUpdate)> Ui for F {
    fn send(&self, update: UiUpdate) {
        self(update);
    }
}

/// The engine as the controller sees it. Every call returns at once; what
/// comes of it arrives later as a `Message`. [`App`] is the real one.
pub trait Engine {
    fn set_window(&self, window: WindowState);
    fn is_acting(&self) -> bool;
    fn refresh_status(&self);
    fn install(&self, base: Option<String>);
    fn start(&self);
    fn stop(&self);
    fn show_list(&self, kind: Option<ResourceKind>);
    fn reload(&self, kind: ResourceKind);
    fn container_action(&self, id: String, action: ContainerAction);
    fn remove(&self, plan: RemovalPlan);
    fn prune(&self, target: PruneTarget);
    fn pull(&self, typed: &str);
    /// Follows a container's logs into `log` and returns the token that
    /// names the log in `stop_logs` and in its last message.
    fn follow_logs(&self, id: String, log: SharedLog) -> u32;
    fn stop_logs(&self, token: u32);
    fn set_usage(&self, shown: bool);
    fn load_disk_usage(&self);
}

impl Engine for App {
    fn set_window(&self, window: WindowState) {
        App::set_window(self, window);
    }

    fn is_acting(&self) -> bool {
        App::is_acting(self)
    }

    fn refresh_status(&self) {
        App::refresh_status(self);
    }

    fn install(&self, base: Option<String>) {
        App::install(self, base);
    }

    fn start(&self) {
        App::start(self);
    }

    fn stop(&self) {
        App::stop(self);
    }

    fn show_list(&self, kind: Option<ResourceKind>) {
        App::show_list(self, kind);
    }

    fn reload(&self, kind: ResourceKind) {
        App::reload(self, kind);
    }

    fn container_action(&self, id: String, action: ContainerAction) {
        App::container_action(self, id, action);
    }

    fn remove(&self, plan: RemovalPlan) {
        App::remove(self, plan);
    }

    fn prune(&self, target: PruneTarget) {
        App::prune(self, target);
    }

    fn pull(&self, typed: &str) {
        App::pull(self, typed);
    }

    fn follow_logs(&self, id: String, log: SharedLog) -> u32 {
        App::follow_logs_into(self, id, log)
    }

    fn stop_logs(&self, token: u32) {
        App::stop_logs(self, token);
    }

    fn set_usage(&self, shown: bool) {
        App::set_usage(self, shown);
    }

    fn load_disk_usage(&self) {
        App::load_disk_usage(self);
    }
}

/// What a confirmation that is on screen was asked about.
enum Pending {
    Remove(RemovalPlan),
    Prune(PruneTarget),
}

/// The log the window shows.
struct OpenLog {
    id: String,
    title: String,
    /// `None` while nothing follows it: the window is away.
    token: Option<u32>,
}

/// What was last sent, so that only changes are sent again.
#[derive(Default)]
struct Sent {
    page: Option<view::Page>,
    header: Option<view::Header>,
    banner: Option<Option<view::Banner>>,
    tray: Option<view::TrayState>,
    usage: Option<Vec<view::Meter>>,
    usage_generation: Option<u64>,
    /// Per tab: the generations of the list and of the disk usage, and
    /// whether the list was loaded, that the table sent last was made from.
    tables: [Option<(u64, u64, bool)>; 4],
    entry: Option<view::Entry>,
}

impl Sent {
    /// The window lost what only it kept: lists, meters and what was typed.
    fn window_gone(&mut self) {
        self.tables = [None; 4];
        self.usage = None;
        self.usage_generation = None;
        self.entry = None;
    }
}

pub struct Controller<E: Engine, U: Ui> {
    engine: E,
    ui: U,
    /// The window is shown and not minimised.
    visible: bool,
    /// What the last poll answered.
    status: Option<EngineStatus>,
    /// Why the engine could not be looked at, while nothing is known.
    status_error: Option<String>,
    /// A poll answered since the window was last shown: `status` is news,
    /// not memory.
    fresh: bool,
    /// The lists are being shown from a running engine.
    listing: bool,
    /// A start or a stop this window asked for, until it is over.
    requested: Option<Transition>,
    usage_on: bool,
    tab: usize,
    lists: [ListModel; 4],
    /// The lists as the engine returned them, to build their tables again
    /// when the disk usage arrives.
    raw: [Option<List>; 4],
    usage: UsageModel,
    /// Counts the changes of Docker's disk usage.
    disk_generation: u64,
    /// The user asked for the disk usage and waits for it.
    disk_asked: bool,
    installing: bool,
    install_base: usize,
    install_steps: Vec<String>,
    pulling: bool,
    pull_status: String,
    error: Option<String>,
    info: Option<String>,
    mirrored_dismissed: bool,
    pending: HashMap<u64, Pending>,
    next_token: u64,
    log: Option<OpenLog>,
    sent: Sent,
}

impl<E: Engine, U: Ui> Controller<E, U> {
    /// A controller for a window that is not shown yet. Nothing is asked of
    /// the engine until it is.
    pub fn new(engine: E, ui: U) -> Self {
        let mut controller = Self {
            engine,
            ui,
            visible: false,
            status: None,
            status_error: None,
            fresh: false,
            listing: false,
            requested: None,
            usage_on: false,
            tab: 0,
            lists: TABS.map(ListModel::new),
            raw: [None, None, None, None],
            usage: UsageModel::default(),
            disk_generation: 0,
            disk_asked: false,
            installing: false,
            install_base: 0,
            install_steps: Vec::new(),
            pulling: false,
            pull_status: String::new(),
            error: None,
            info: None,
            mirrored_dismissed: false,
            pending: HashMap::new(),
            next_token: 1,
            log: None,
            sent: Sent::default(),
        };
        controller.ui.send(UiUpdate::Tabs(present::tab_titles()));
        controller.sync();
        controller
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// The list a tab shows.
    pub fn list(&self, kind: ResourceKind) -> &ListModel {
        &self.lists[present::tab_of(kind)]
    }

    /// Shows a line of information above the content, until it is
    /// dismissed or something else is said.
    pub fn show_info(&mut self, text: impl Into<String>) {
        self.info = Some(text.into());
        self.sync();
    }

    fn state(&self) -> Option<&EngineState> {
        self.status.as_ref().map(|status| &status.state)
    }

    fn kind(&self) -> ResourceKind {
        TABS[self.tab.min(TABS.len() - 1)]
    }

    // -----------------------------------------------------------------
    // What the user does

    /// Answers one event of the window, then sends what changed.
    pub fn on_event(&mut self, event: UiEvent) {
        self.handle_event(event);
        self.sync();
    }

    fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::WindowShown => self.window_shown(),
            UiEvent::WindowHidden => self.window_hidden(),
            UiEvent::TabChanged { tab } if tab < TABS.len() => {
                self.tab = tab;
                self.sent.tables[tab] = None;
                self.send_filter();
                if self.listing {
                    self.engine.show_list(Some(self.kind()));
                }
            }
            UiEvent::FilterChanged { tab, text } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    list.set_filter(&text);
                }
            }
            UiEvent::SortRequested { tab, column } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    list.toggle_sort(column);
                }
            }
            UiEvent::RowToggled { tab, id, range } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    if range {
                        list.select_range_to(&id);
                    } else {
                        list.toggle(&id);
                    }
                }
            }
            UiEvent::GroupToggled { tab, name } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    list.toggle_group_selection(&name);
                }
            }
            UiEvent::GroupFolded { tab, name } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    list.toggle_group(&name);
                }
            }
            UiEvent::AllToggled { tab } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    list.toggle_all();
                }
            }
            UiEvent::SelectionCleared { tab } => {
                if let Some(list) = self.lists.get_mut(tab) {
                    list.clear_selection();
                }
            }
            UiEvent::RowAction { tab, id, action } => self.row_action(tab, id, &action),
            UiEvent::RemoveRequested { tab, id } => self.ask_removal(tab, id.as_deref()),
            UiEvent::Command { tab, key } => self.command(tab, &key),
            UiEvent::Submitted { tab, text } => self.submitted(tab, &text),
            UiEvent::Confirmed { token, accepted } => self.confirmed(token, accepted),
            UiEvent::Button { key } => self.button(&key),
            UiEvent::ChoiceChanged { index } => self.install_base = index,
            UiEvent::BannerDismissed => self.dismiss_banner(),
            UiEvent::UsageClicked => {
                let notice = present::usage_notice(&self.usage.meters());
                self.ui.send(UiUpdate::Notice(notice));
            }
            UiEvent::Tray(TrayCommand::StartEngine) => self.start_engine(),
            UiEvent::Tray(TrayCommand::StopEngine) => self.stop_engine(),
            UiEvent::LogClosed => self.log_closed(),
            UiEvent::Refresh => {
                self.engine.refresh_status();
                if self.listing {
                    self.engine.reload(self.kind());
                }
            }
            // The window opens and quits by itself; a wake-up carries
            // nothing; a tab that does not exist is not a tab.
            UiEvent::Tray(TrayCommand::Open | TrayCommand::Quit)
            | UiEvent::TabChanged { .. }
            | UiEvent::LogOpened
            | UiEvent::Wake => {}
        }
    }

    fn window_shown(&mut self) {
        if self.visible {
            return;
        }
        self.visible = true;
        // What is known is from before the window went away. The poll this
        // starts says what is true now; the lists wait for it.
        self.fresh = false;
        self.sent.window_gone();
        self.ui.send(UiUpdate::Tabs(present::tab_titles()));
        self.send_filter();
        self.engine.set_window(WindowState::Visible);
    }

    fn window_hidden(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.fresh = false;
        self.listing = false;
        self.set_usage(false);
        if let Some(log) = &mut self.log {
            // Ended by the engine below. It is followed again, from its
            // last lines, when the window is back.
            log.token = None;
        }
        // Not `Hidden`: that keeps the event stream, which is a Docker
        // connection and a `wsl.exe` child for as long as it lasts.
        self.engine.set_window(WindowState::Gone);
    }

    fn set_usage(&mut self, shown: bool) {
        if self.usage_on == shown {
            return;
        }
        self.usage_on = shown;
        self.engine.set_usage(shown);
        if !shown {
            // Figures from before would look current when it shows again.
            self.usage.clear();
            self.disk_generation += 1;
        }
    }

    fn send_filter(&self) {
        let text = self.lists[self.tab].filter().to_string();
        self.ui.send(UiUpdate::Filter {
            tab: self.tab,
            text,
        });
    }

    fn button(&mut self, key: &str) {
        match key {
            key::ENGINE_START => self.start_engine(),
            key::ENGINE_STOP => self.stop_engine(),
            key::REFRESH => {
                self.status_error = None;
                self.engine.refresh_status();
            }
            key::INSTALL if !self.installing => {
                let bases = self.status.as_ref().map(|status| status.bases.clone());
                let base = bases.and_then(|bases| bases.get(self.install_base).copied());
                self.installing = true;
                self.install_steps.clear();
                self.error = None;
                self.engine.install(base.map(str::to_string));
            }
            key::COPY_COMMAND => {
                let command = present::copied_command().to_string();
                self.ui.send(UiUpdate::Copy(command));
            }
            _ => {}
        }
    }

    fn start_engine(&mut self) {
        if self.engine.is_acting() {
            return;
        }
        self.error = None;
        self.requested = Some(Transition::Starting);
        self.engine.start();
    }

    fn stop_engine(&mut self) {
        if self.engine.is_acting() {
            return;
        }
        self.error = None;
        self.requested = Some(Transition::Stopping);
        // The engine is on its way out: nothing more is asked of it.
        self.leave_lists();
        self.engine.stop();
    }

    fn row_action(&mut self, tab: usize, id: String, action: &str) {
        if !self.listing || tab >= TABS.len() {
            return;
        }
        if let Some(port) = present::port_of(action) {
            // Only a port the row really publishes.
            let publishes = |row: &crate::table::Row| row.ports.contains(&port);
            if self.lists[tab].row(&id).is_some_and(publishes) {
                self.ui.send(UiUpdate::OpenPort(port));
            }
            return;
        }
        match action {
            key::REMOVE => self.ask_removal(tab, Some(&id)),
            key::OPEN => {
                let row = self.lists[tab].row(&id);
                if let Some(&port) = row.and_then(|row| row.ports.first()) {
                    self.ui.send(UiUpdate::OpenPort(port));
                }
            }
            key::LOGS if TABS[tab] == ResourceKind::Container => self.open_log(id),
            key::START | key::STOP | key::RESTART if TABS[tab] == ResourceKind::Container => {
                let Ok(action) = action.parse::<ContainerAction>() else {
                    return;
                };
                if self.lists[tab].is_busy(&id) || self.lists[tab].row(&id).is_none() {
                    return;
                }
                self.lists[tab].set_busy(&id, true);
                self.engine.container_action(id, action);
            }
            _ => {}
        }
    }

    /// Asks before removing the row named, or else the selection.
    fn ask_removal(&mut self, tab: usize, id: Option<&str>) {
        if !self.listing || tab >= TABS.len() {
            return;
        }
        let list = &self.lists[tab];
        let plan = match id {
            Some(id) => RemovalPlan::new(TABS[tab], list.row(id)),
            None => list.removal_plan(),
        };
        if plan.is_empty() {
            return;
        }
        let token = self.token();
        let confirm = present::removal_confirm(token, &plan);
        self.pending.insert(token, Pending::Remove(plan));
        self.ui.send(UiUpdate::Confirm(confirm));
    }

    fn token(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token += 1;
        token
    }

    fn command(&mut self, tab: usize, command: &str) {
        if !self.listing || tab >= TABS.len() {
            return;
        }
        if let Some(target) = present::prune_of(TABS[tab], command) {
            let token = self.token();
            self.pending.insert(token, Pending::Prune(target));
            let confirm = present::prune_confirm(token, target);
            self.ui.send(UiUpdate::Confirm(confirm));
        } else if command == key::DISK_USAGE
            && loads_disk_usage(DiskUsageTrigger::Requested, self.state())
            && !self.usage.disk_is_loading()
        {
            self.disk_asked = true;
            self.usage.disk_loading();
            self.disk_generation += 1;
            self.engine.load_disk_usage();
        }
    }

    fn confirmed(&mut self, token: u64, accepted: bool) {
        let Some(pending) = self.pending.remove(&token) else {
            return;
        };
        // The answer may come long after the question: the engine may have
        // stopped meanwhile.
        if !accepted || !self.listing {
            return;
        }
        match pending {
            Pending::Remove(plan) => self.engine.remove(plan),
            Pending::Prune(target) => self.engine.prune(target),
        }
    }

    fn submitted(&mut self, tab: usize, text: &str) {
        let images = TABS.get(tab) == Some(&ResourceKind::Image);
        if !self.listing || !images || !can_pull(text, self.pulling) {
            return;
        }
        self.pulling = true;
        self.pull_status = format!("Pulling {}…", text.trim());
        self.error = None;
        self.engine.pull(text);
    }

    fn dismiss_banner(&mut self) {
        // The banner shows one of these at a time, in this order.
        if self.error.take().is_some() || self.info.take().is_some() {
            return;
        }
        self.mirrored_dismissed = true;
    }

    fn open_log(&mut self, id: String) {
        let Some(row) = self.lists[0].row(&id) else {
            return;
        };
        let title = row.name.clone();
        self.close_log();
        self.log = Some(OpenLog {
            id,
            title,
            token: None,
        });
        self.follow_log();
    }

    /// Follows the open log from its last lines, into a fresh buffer.
    fn follow_log(&mut self) {
        let Some(log) = &mut self.log else {
            return;
        };
        let buffer = SharedLog::new();
        log.token = Some(self.engine.follow_logs(log.id.clone(), buffer.clone()));
        self.ui.send(UiUpdate::ShowLog(view::LogPane {
            title: log.title.clone(),
            source: Arc::new(buffer),
        }));
    }

    /// Ends the open log, if there is one, and takes its view down.
    fn close_log(&mut self) {
        if let Some(log) = self.log.take() {
            if let Some(token) = log.token {
                self.engine.stop_logs(token);
            }
            self.ui.send(UiUpdate::CloseLog);
        }
    }

    /// The window took the log view down.
    fn log_closed(&mut self) {
        if let Some(token) = self.log.take().and_then(|log| log.token) {
            self.engine.stop_logs(token);
        }
    }

    // -----------------------------------------------------------------
    // What the engine says

    /// Takes in one message of the engine, then sends what changed.
    pub fn on_message(&mut self, message: Message) {
        self.handle_message(message);
        self.sync();
    }

    /// Takes in a message without sending anything yet: call [`Self::sync`]
    /// after the last one of a batch.
    pub fn handle_message(&mut self, message: Message) {
        match message {
            Message::Status(Ok(status)) => self.status_arrived(status),
            Message::Status(Err(error)) => {
                if self.status.is_none() {
                    self.status_error = Some(error);
                } else {
                    self.fail(format!("The engine state could not be read: {error}"));
                }
            }
            Message::InstallProgress(step) => {
                self.installing = true;
                self.install_steps.push(step);
            }
            Message::EngineRequestDone { transition, error } => {
                if transition == Transition::Installing {
                    self.installing = false;
                } else if self.requested == Some(transition) {
                    self.requested = None;
                }
                if let Some(error) = error {
                    self.fail(error);
                }
            }
            Message::List { kind, result } => match result {
                Ok(list) => {
                    let tab = present::tab_of(kind);
                    let table = list.table(self.usage.containers(), self.usage.disk());
                    self.lists[tab].set_table(table);
                    self.raw[tab] = Some(list);
                }
                Err(error) => self.fail(error),
            },
            Message::ActionDone { id, error, .. } => {
                self.lists[0].set_busy(&id, false);
                if let Some(error) = error {
                    self.fail(error);
                }
            }
            Message::Removed { plan, report } => {
                self.lists[present::tab_of(plan.kind)].clear_selection();
                self.forget_disk();
                if report.failed() == 0 {
                    self.info = Some(removal_summary(&plan, &report));
                } else {
                    let notice = present::removal_notice(&plan, &report);
                    self.ui.send(UiUpdate::Notice(notice));
                }
            }
            Message::Pruned { target, result } => match result {
                Ok(report) => {
                    self.forget_disk();
                    self.info = Some(prune_summary(target, &report));
                }
                Err(error) => self.fail(error),
            },
            Message::PullProgress(line) => self.pull_status = line,
            Message::PullDone { reference, error } => {
                self.pulling = false;
                self.pull_status.clear();
                match error {
                    Some(error) => self.fail(error),
                    None => {
                        self.forget_disk();
                        self.info = Some(format!("Pulled {reference}."));
                        // Empties the box for the next image.
                        let entry = view::Entry {
                            text: Some(String::new()),
                            ..present::pull_entry(false, "")
                        };
                        self.sent.entry = Some(entry.clone());
                        self.ui.send(UiUpdate::Entry {
                            tab: present::tab_of(ResourceKind::Image),
                            entry: Some(entry),
                        });
                    }
                }
            }
            Message::Log { token, message } => {
                let ours = self
                    .log
                    .as_ref()
                    .is_some_and(|log| log.token == Some(token));
                if let (true, LogMessage::Ended { error }) = (ours, message) {
                    let note = match error {
                        None => "log ended".to_string(),
                        Some(error) => format!("log stopped: {error}"),
                    };
                    self.ui.send(UiUpdate::LogNote(note));
                }
            }
            Message::HostUsage(reading) => {
                if self.usage_on {
                    self.usage.set_host(reading);
                }
            }
            Message::ContainerUsage(usage) => {
                if self.usage_on {
                    self.usage.set_containers(usage);
                    self.lists[0].set_usage(self.usage.containers());
                }
            }
            Message::DiskUsage(result) => self.disk_usage_arrived(result),
            // The lists reload by themselves.
            Message::Docker(_) => {}
        }
    }

    fn status_arrived(&mut self, status: EngineStatus) {
        self.status_error = None;
        let runs = uses_docker_api(&status.state);
        if status.state != EngineState::NotInstalled {
            self.install_steps.clear();
        }
        self.status = Some(status);
        // A poll the window did not ask for (the tray's start or stop) says
        // nothing about what to show.
        if !self.visible {
            return;
        }
        self.fresh = true;
        let has_engine = !matches!(
            self.state(),
            None | Some(EngineState::WslMissing | EngineState::NotInstalled)
        );
        // The host's figures are read from Windows alone, whatever the
        // engine does.
        self.set_usage(has_engine);
        if !runs {
            self.leave_lists();
        } else if !self.listing && self.requested != Some(Transition::Stopping) {
            self.listing = true;
            self.engine.show_list(Some(self.kind()));
            if self.log.is_some() {
                self.follow_log();
            }
        }
    }

    /// The engine does not run any more, or is about to stop.
    fn leave_lists(&mut self) {
        // Every poll of an engine that does not run comes here; only the
        // first one after it ran has anything to take down.
        let holds = self.listing
            || self.log.is_some()
            || self.pulling
            || !self.pending.is_empty()
            || self.raw.iter().any(Option::is_some);
        if !holds {
            return;
        }
        self.listing = false;
        self.close_log();
        self.pending.clear();
        self.pulling = false;
        self.pull_status.clear();
        self.usage.engine_left();
        self.disk_generation += 1;
        self.disk_asked = false;
        self.raw = [None, None, None, None];
        self.lists = TABS.map(|kind| {
            // The filter is the user's; the rows are the engine's.
            let mut list = ListModel::new(kind);
            list.set_filter(self.lists[present::tab_of(kind)].filter());
            list
        });
        self.sent.tables = [None; 4];
    }

    /// Something was removed or added: Docker's disk usage is from before.
    fn forget_disk(&mut self) {
        let before = self.usage.generation();
        self.usage.forget_disk();
        if self.usage.generation() != before {
            self.disk_generation += 1;
        }
    }

    fn disk_usage_arrived(&mut self, result: Result<docklet_engine::DiskUsage, String>) {
        let asked = std::mem::take(&mut self.disk_asked);
        if !self.listing {
            return;
        }
        match (&result, asked) {
            (Ok(usage), true) => self.ui.send(UiUpdate::Notice(present::disk_notice(usage))),
            (Err(error), true) => self.fail(format!("Disk usage could not be measured: {error}")),
            (_, false) => {}
        }
        self.usage.set_disk(result);
        self.disk_generation += 1;
        // The sizes of the volumes come with it.
        let volumes = present::tab_of(ResourceKind::Volume);
        if let Some(list) = &self.raw[volumes] {
            let table = list.table(None, self.usage.disk());
            self.lists[volumes].set_table(table);
        }
    }

    /// Something failed. Shown above the content, or in a box of its own
    /// when there is no window to show it in (a start from the tray).
    fn fail(&mut self, error: String) {
        if self.visible {
            self.info = None;
            self.error = Some(error);
        } else {
            self.ui.send(UiUpdate::Notice(view::Notice {
                title: "Docklet".to_string(),
                message: error,
                detail: None,
                tone: view::Tone::Error,
            }));
        }
    }

    // -----------------------------------------------------------------
    // What to draw

    fn page(&self) -> view::Page {
        let acting = self.engine.is_acting();
        let Some(status) = &self.status else {
            return view::Page::message(match &self.status_error {
                Some(error) => present::engine_screen(&EngineState::Error(error.clone()), acting),
                None => present::connecting_screen(),
            });
        };
        // The window's own start or stop shows at once, not with the next
        // poll.
        let state = match (self.requested, &status.state) {
            (Some(Transition::Starting), state) if !uses_docker_api(state) => {
                &EngineState::Starting
            }
            (Some(Transition::Stopping), _) => &EngineState::Stopping,
            (_, state) => state,
        };
        match state {
            EngineState::Running => view::Page::Tables,
            EngineState::NotInstalled => view::Page::message(present::install_screen(
                &status.bases,
                self.install_base,
                self.installing,
                &self.install_steps,
            )),
            state => view::Page::message(present::engine_screen(state, acting)),
        }
    }

    fn banner(&self) -> Option<view::Banner> {
        let banner = |text: &str, tone| view::Banner {
            text: text.to_string(),
            tone,
        };
        if let Some(error) = &self.error {
            return Some(banner(error, view::Tone::Error));
        }
        if let Some(info) = &self.info {
            return Some(banner(info, view::Tone::Info));
        }
        let mirrored = self
            .status
            .as_ref()
            .is_some_and(|status| status.mirrored_networking);
        (mirrored && !self.mirrored_dismissed)
            .then(|| banner(MIRRORED_NETWORKING_WARNING, view::Tone::Warning))
    }

    /// Sends the window everything that differs from what it was sent
    /// last. The lists and the meters only while it is shown: a window that
    /// is away holds none.
    pub fn sync(&mut self) {
        let acting = self.engine.is_acting();
        let page = self.page();
        if self.sent.page.as_ref() != Some(&page) {
            self.sent.page = Some(page.clone());
            self.ui.send(UiUpdate::Page(page));
        }
        let header = present::header_view(self.state(), acting);
        if self.sent.header.as_ref() != Some(&header) {
            self.sent.header = Some(header.clone());
            self.ui.send(UiUpdate::Header(header));
        }
        let banner = self.banner();
        if self.sent.banner.as_ref() != Some(&banner) {
            self.sent.banner = Some(banner.clone());
            self.ui.send(UiUpdate::Banner(banner));
        }
        // While nobody looks, what the engine does is not known.
        let known = self.state().filter(|_| self.fresh);
        let tray = present::tray_view(known, acting);
        if self.sent.tray.as_ref() != Some(&tray) {
            self.sent.tray = Some(tray.clone());
            self.ui.send(UiUpdate::Tray(tray));
        }
        if !self.visible {
            return;
        }
        self.sync_usage();
        self.sync_table();
        self.sync_entry();
    }

    fn sync_usage(&mut self) {
        let generation = self.usage.generation();
        if self.sent.usage_generation == Some(generation) && self.sent.usage.is_some() {
            return;
        }
        self.sent.usage_generation = Some(generation);
        let meters = if self.usage_on {
            present::meters_view(&self.usage.meters())
        } else {
            Vec::new()
        };
        if self.sent.usage.as_ref() != Some(&meters) {
            self.sent.usage = Some(meters.clone());
            self.ui.send(UiUpdate::Usage(meters));
        }
    }

    fn sync_table(&mut self) {
        // A message screen shows no table.
        if self.sent.page != Some(view::Page::Tables) {
            return;
        }
        let tab = self.tab;
        let list = &self.lists[tab];
        let loaded = self.raw[tab].is_some();
        let made_from = (list.generation(), self.disk_generation, loaded);
        if self.sent.tables[tab] == Some(made_from) {
            return;
        }
        self.sent.tables[tab] = Some(made_from);
        let table = present::table_view(list, loaded, self.usage.disk());
        self.ui.send(UiUpdate::Table { tab, table });
    }

    fn sync_entry(&mut self) {
        let entry = present::pull_entry(self.pulling, &self.pull_status);
        // What was sent to empty the box is this entry too.
        let same = self.sent.entry.as_ref().is_some_and(|sent| {
            let sent = view::Entry {
                text: None,
                ..sent.clone()
            };
            sent == entry
        });
        if !same {
            self.sent.entry = Some(entry.clone());
            self.ui.send(UiUpdate::Entry {
                tab: present::tab_of(ResourceKind::Image),
                entry: Some(entry),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use docklet_engine::{DiskUsage, PruneReport, RemoveFailure, RemoveReport, RemoveResult};

    use super::*;
    use crate::streams::HostReading;
    use crate::table::testing::{container, image, usage, volume};

    /// An engine that records what it is asked.
    #[derive(Default)]
    struct FakeEngine {
        calls: RefCell<Vec<String>>,
        acting: RefCell<bool>,
        logs: RefCell<Vec<SharedLog>>,
    }

    impl FakeEngine {
        fn record(&self, call: impl Into<String>) {
            self.calls.borrow_mut().push(call.into());
        }
    }

    impl Engine for Rc<FakeEngine> {
        fn set_window(&self, window: WindowState) {
            self.record(format!("window {window:?}"));
        }

        fn is_acting(&self) -> bool {
            *self.acting.borrow()
        }

        fn refresh_status(&self) {
            self.record("refresh");
        }

        fn install(&self, base: Option<String>) {
            self.record(format!("install {}", base.unwrap_or_default()));
        }

        fn start(&self) {
            *self.acting.borrow_mut() = true;
            self.record("start");
        }

        fn stop(&self) {
            *self.acting.borrow_mut() = true;
            self.record("stop");
        }

        fn show_list(&self, kind: Option<ResourceKind>) {
            self.record(format!("show {}", kind.map_or("none", ResourceKind::name)));
        }

        fn reload(&self, kind: ResourceKind) {
            self.record(format!("reload {}", kind.name()));
        }

        fn container_action(&self, id: String, action: ContainerAction) {
            self.record(format!("{action:?} {id}"));
        }

        fn remove(&self, plan: RemovalPlan) {
            self.record(format!("remove {}", plan.ids().join(",")));
        }

        fn prune(&self, target: PruneTarget) {
            self.record(format!("prune {target:?}"));
        }

        fn pull(&self, typed: &str) {
            self.record(format!("pull {typed}"));
        }

        fn follow_logs(&self, id: String, log: SharedLog) -> u32 {
            self.record(format!("follow {id}"));
            self.logs.borrow_mut().push(log);
            self.logs.borrow().len() as u32
        }

        fn stop_logs(&self, token: u32) {
            self.record(format!("unfollow {token}"));
        }

        fn set_usage(&self, shown: bool) {
            self.record(format!("usage {shown}"));
        }

        fn load_disk_usage(&self) {
            self.record("disk");
        }
    }

    type Updates = Rc<RefCell<Vec<UiUpdate>>>;

    /// A window that keeps what it is sent.
    struct Recorder(Updates);

    impl Ui for Recorder {
        fn send(&self, update: UiUpdate) {
            self.0.borrow_mut().push(update);
        }
    }

    struct Harness {
        controller: Controller<Rc<FakeEngine>, Recorder>,
        engine: Rc<FakeEngine>,
        updates: Updates,
    }

    fn harness() -> Harness {
        let engine = Rc::new(FakeEngine::default());
        let updates = Updates::default();
        Harness {
            controller: Controller::new(engine.clone(), Recorder(updates.clone())),
            engine,
            updates,
        }
    }

    fn status(state: EngineState) -> Message {
        Message::Status(Ok(EngineStatus {
            state,
            bases: vec!["debian-13", "alpine"],
            mirrored_networking: false,
        }))
    }

    fn containers() -> Message {
        Message::List {
            kind: ResourceKind::Container,
            result: Ok(List::Containers(vec![
                container("web", Some("shop"), true),
                container("db", Some("shop"), false),
                container("solo", None, true),
            ])),
        }
    }

    impl Harness {
        /// What the engine was asked since the last look.
        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut *self.engine.calls.borrow_mut())
        }

        /// What the window was sent since the last look.
        fn updates(&self) -> Vec<UiUpdate> {
            std::mem::take(&mut *self.updates.borrow_mut())
        }

        fn event(&mut self, event: UiEvent) {
            self.controller.on_event(event);
        }

        fn message(&mut self, message: Message) {
            self.controller.on_message(message);
        }

        /// A window that shows the containers of a running engine.
        fn running() -> Self {
            let mut harness = harness();
            harness.event(UiEvent::WindowShown);
            harness.message(status(EngineState::Running));
            harness.message(containers());
            harness.calls();
            harness.updates();
            harness
        }

        fn page(&self) -> view::Page {
            self.controller.page()
        }

        fn screen(&self) -> view::Screen {
            match self.page() {
                view::Page::Message(screen) => *screen,
                view::Page::Tables => panic!("the tables show"),
            }
        }

        /// The newest of what the window was sent that `pick` takes.
        fn last<T>(&self, pick: impl Fn(&UiUpdate) -> Option<T>) -> Option<T> {
            self.updates.borrow().iter().rev().find_map(pick)
        }

        /// The table last sent for `tab`.
        fn table(&self, tab: usize) -> view::Table {
            let table = self.last(|update| match update {
                UiUpdate::Table { tab: sent, table } if *sent == tab => Some(table.clone()),
                _ => None,
            });
            table.expect("a table was sent")
        }

        fn banner(&self) -> Option<view::Banner> {
            self.controller.banner()
        }

        fn confirm(&self) -> view::Confirm {
            let confirm = self.last(|update| match update {
                UiUpdate::Confirm(confirm) => Some(confirm.clone()),
                _ => None,
            });
            confirm.expect("a question was asked")
        }

        fn notice(&self) -> Option<view::Notice> {
            self.last(|update| match update {
                UiUpdate::Notice(notice) => Some(notice.clone()),
                _ => None,
            })
        }

        /// The line to type on the images tab, as last sent.
        fn entry(&self) -> view::Entry {
            let entry = self.last(|update| match update {
                UiUpdate::Entry { tab: 1, entry } => entry.clone(),
                _ => None,
            });
            entry.expect("an entry was sent")
        }

        fn pane(&self) -> view::LogPane {
            let pane = self.last(|update| match update {
                UiUpdate::ShowLog(pane) => Some(pane.clone()),
                _ => None,
            });
            pane.expect("a log was shown")
        }

        fn meters(&self) -> Vec<view::Meter> {
            let meters = self.last(|update| match update {
                UiUpdate::Usage(meters) => Some(meters.clone()),
                _ => None,
            });
            meters.expect("meters were sent")
        }
    }

    fn row_ids(table: &view::Table) -> Vec<String> {
        table
            .lines
            .iter()
            .filter_map(|line| match line {
                view::Line::Row(row) => Some(row.id.clone()),
                view::Line::Group { .. } => None,
            })
            .collect()
    }

    #[test]
    fn nothing_is_asked_of_the_engine_before_there_is_a_window() {
        let harness = harness();
        assert_eq!(harness.calls(), [] as [&str; 0]);
        assert_eq!(harness.screen().title, "Connecting…");
        let updates = harness.updates();
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, UiUpdate::Tabs(tabs) if tabs.len() == 4))
        );
        // No list and no meter is sent to a window that is not there.
        assert!(
            !updates
                .iter()
                .any(|update| matches!(update, UiUpdate::Table { .. } | UiUpdate::Usage(_)))
        );
    }

    #[test]
    fn the_lists_wait_for_the_poll_that_says_the_engine_runs() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        assert_eq!(harness.calls(), ["window Visible"]);
        harness.message(status(EngineState::Running));
        assert_eq!(harness.calls(), ["usage true", "show container"]);
        assert_eq!(harness.page(), view::Page::Tables);
        assert_eq!(harness.table(0).summary, "Loading…");
        harness.message(containers());
        assert_eq!(harness.table(0).summary, "3 containers");
        // Another poll with the same answer asks for nothing again.
        harness.message(status(EngineState::Running));
        assert_eq!(harness.calls(), [] as [&str; 0]);
    }

    #[test]
    fn a_sleeping_engine_is_shown_and_left_alone() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        harness.message(status(EngineState::Sleeping));
        assert_eq!(harness.screen().title, "The engine is asleep");
        // The host's figures are read from Windows; no list, no log.
        assert_eq!(harness.calls(), ["window Visible", "usage true"]);
        harness.event(UiEvent::Refresh);
        harness.event(UiEvent::TabChanged { tab: 1 });
        harness.event(UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "logs".to_string(),
        });
        harness.event(UiEvent::Command {
            tab: 1,
            key: "disk-usage".to_string(),
        });
        harness.event(UiEvent::Submitted {
            tab: 1,
            text: "nginx".to_string(),
        });
        assert_eq!(harness.calls(), ["refresh"]);
    }

    #[test]
    fn a_hidden_window_holds_nothing_and_asks_again_only_after_a_fresh_poll() {
        let mut harness = Harness::running();
        harness.event(UiEvent::WindowHidden);
        assert_eq!(harness.calls(), ["usage false", "window Gone"]);
        // Nothing of what the engine still sends reaches a window that is away.
        harness.message(containers());
        harness.message(Message::HostUsage(HostReading::Failed("x".to_string())));
        assert!(
            !harness
                .updates()
                .iter()
                .any(|update| matches!(update, UiUpdate::Table { .. } | UiUpdate::Usage(_)))
        );

        harness.event(UiEvent::WindowShown);
        // Only the poll: what was known is from before.
        assert_eq!(harness.calls(), ["window Visible"]);
        harness.event(UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "stop".to_string(),
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
        // It went to sleep while nobody looked: it stays asleep.
        harness.message(status(EngineState::Sleeping));
        assert_eq!(harness.calls(), ["usage true"]);
        assert_eq!(harness.screen().title, "The engine is asleep");
    }

    #[test]
    fn a_window_that_comes_back_to_a_running_engine_gets_its_lists_again() {
        let mut harness = Harness::running();
        harness.event(UiEvent::WindowHidden);
        harness.updates();
        harness.event(UiEvent::WindowShown);
        // The window dropped its lists; it gets the one it shows again, as
        // it was, until the engine has answered.
        assert_eq!(harness.table(0).summary, "3 containers");
        harness.calls();
        harness.message(status(EngineState::Running));
        assert_eq!(harness.calls(), ["usage true", "show container"]);
    }

    #[test]
    fn hiding_twice_or_showing_twice_changes_nothing() {
        let mut harness = harness();
        harness.event(UiEvent::WindowHidden);
        assert_eq!(harness.calls(), [] as [&str; 0]);
        harness.event(UiEvent::WindowShown);
        harness.event(UiEvent::WindowShown);
        assert_eq!(harness.calls(), ["window Visible"]);
    }

    #[test]
    fn each_tab_shows_its_own_list_and_filter() {
        let mut harness = Harness::running();
        harness.event(UiEvent::FilterChanged {
            tab: 0,
            text: "solo".to_string(),
        });
        assert_eq!(row_ids(&harness.table(0)), ["id-solo"]);
        assert_eq!(harness.table(0).summary, "1 of 3 containers");
        harness.updates();

        harness.event(UiEvent::TabChanged { tab: 1 });
        assert_eq!(harness.calls(), ["show image"]);
        let updates = harness.updates();
        assert!(updates.iter().any(|update| matches!(
            update,
            UiUpdate::Filter { tab: 1, text } if text.is_empty()
        )));
        harness.message(Message::List {
            kind: ResourceKind::Image,
            result: Ok(List::Images(vec![image("sha256:a", &["nginx:1"], 1, 0)])),
        });
        assert_eq!(harness.table(1).summary, "1 image");
        harness.updates();

        // Back on the first tab the filter is still there.
        harness.event(UiEvent::TabChanged { tab: 0 });
        let updates = harness.updates();
        assert!(updates.iter().any(|update| matches!(
            update,
            UiUpdate::Filter { tab: 0, text } if text == "solo"
        )));
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, UiUpdate::Table { tab: 0, .. }))
        );
    }

    #[test]
    fn select_all_and_remove_take_only_the_rows_on_screen() {
        let mut harness = Harness::running();
        harness.event(UiEvent::FilterChanged {
            tab: 0,
            text: "shop".to_string(),
        });
        harness.event(UiEvent::AllToggled { tab: 0 });
        let table = harness.table(0);
        assert_eq!(table.selected, 2);
        assert_eq!(table.select_all, view::Coverage::All);
        assert_eq!(table.summary, "2 selected");

        harness.event(UiEvent::RemoveRequested { tab: 0, id: None });
        let confirm = harness.confirm();
        assert_eq!(confirm.title, "Remove 2 containers?");
        assert_eq!(
            confirm.message,
            "1 is running and will be stopped. This cannot be undone."
        );
        // Nothing is removed before the answer.
        assert_eq!(harness.calls(), [] as [&str; 0]);
        harness.event(UiEvent::Confirmed {
            token: confirm.token,
            accepted: true,
        });
        // `solo` is hidden by the filter and stays.
        assert_eq!(harness.calls(), ["remove id-db,id-web"]);
        // The same answer again removes nothing twice.
        harness.event(UiEvent::Confirmed {
            token: confirm.token,
            accepted: true,
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
    }

    #[test]
    fn a_collapsed_group_is_left_out_of_select_all() {
        let mut harness = Harness::running();
        harness.event(UiEvent::GroupFolded {
            tab: 0,
            name: "shop".to_string(),
        });
        harness.event(UiEvent::AllToggled { tab: 0 });
        assert_eq!(harness.table(0).selected, 1);
        harness.event(UiEvent::RemoveRequested { tab: 0, id: None });
        assert_eq!(harness.confirm().title, "Remove container solo?");
    }

    #[test]
    fn a_cancelled_removal_removes_nothing() {
        let mut harness = Harness::running();
        harness.event(UiEvent::RemoveRequested {
            tab: 0,
            id: Some("id-web".to_string()),
        });
        let confirm = harness.confirm();
        assert_eq!(confirm.title, "Remove container web?");
        assert_eq!(confirm.accept, "Remove");
        harness.event(UiEvent::Confirmed {
            token: confirm.token,
            accepted: false,
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
        // Nothing selected and no row named: nothing to ask.
        harness.updates();
        harness.event(UiEvent::RemoveRequested { tab: 0, id: None });
        assert!(harness.updates().is_empty());
    }

    #[test]
    fn the_result_of_a_removal_is_said_in_words() {
        let mut harness = Harness::running();
        harness.event(UiEvent::AllToggled { tab: 0 });
        let plan = harness
            .controller
            .list(ResourceKind::Container)
            .removal_plan();
        let removed = |id: &str| RemoveResult::removed(ResourceKind::Container, id);
        harness.message(Message::Removed {
            plan: plan.clone(),
            report: RemoveReport {
                results: vec![removed("id-db"), removed("id-web"), removed("id-solo")],
            },
        });
        let banner = harness.banner().unwrap();
        assert_eq!(banner.text, "Removed 3 containers.");
        assert_eq!(banner.tone, view::Tone::Info);
        assert_eq!(harness.table(0).selected, 0);
        assert_eq!(harness.notice(), None);

        let failed = RemoveResult::failed(
            ResourceKind::Container,
            "id-web",
            RemoveFailure::Other,
            "driver failed",
        );
        harness.message(Message::Removed {
            plan,
            report: RemoveReport {
                results: vec![removed("id-db"), failed, removed("id-solo")],
            },
        });
        let notice = harness.notice().unwrap();
        assert!(notice.message.starts_with("Removed 2. 1 failed: web."));
        assert_eq!(notice.detail.as_deref(), Some("web: driver failed"));
    }

    #[test]
    fn a_prune_is_confirmed_and_says_what_it_reclaimed() {
        let mut harness = Harness::running();
        harness.event(UiEvent::Command {
            tab: 0,
            key: "prune:0".to_string(),
        });
        let confirm = harness.confirm();
        assert_eq!(confirm.title, "Remove all stopped containers?");
        harness.event(UiEvent::Confirmed {
            token: confirm.token,
            accepted: true,
        });
        assert_eq!(harness.calls(), ["prune StoppedContainers"]);
        harness.message(Message::Pruned {
            target: PruneTarget::StoppedContainers,
            result: Ok(PruneReport {
                deleted: vec!["a".to_string(), "b".to_string()],
                untagged: Vec::new(),
                space_reclaimed: 1_200_000_000,
            }),
        });
        assert_eq!(
            harness.banner().unwrap().text,
            "Removed 2 stopped containers, reclaimed 1.2 GB."
        );
        harness.message(Message::Pruned {
            target: PruneTarget::StoppedContainers,
            result: Err("no answer".to_string()),
        });
        let banner = harness.banner().unwrap();
        assert_eq!(
            (banner.text.as_str(), banner.tone),
            ("no answer", view::Tone::Error)
        );
    }

    #[test]
    fn a_container_action_marks_its_row_until_it_is_over() {
        let mut harness = Harness::running();
        let stop = UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "stop".to_string(),
        };
        harness.event(stop.clone());
        assert_eq!(harness.calls(), ["Stop id-web"]);
        let busy = |harness: &Harness| {
            harness
                .controller
                .list(ResourceKind::Container)
                .is_busy("id-web")
        };
        assert!(busy(&harness));
        // A second click while it runs does nothing.
        harness.event(stop);
        assert_eq!(harness.calls(), [] as [&str; 0]);
        harness.message(Message::ActionDone {
            id: "id-web".to_string(),
            action: ContainerAction::Stop,
            error: Some("cannot stop".to_string()),
        });
        assert!(!busy(&harness));
        assert_eq!(harness.banner().unwrap().text, "cannot stop");
        harness.event(UiEvent::BannerDismissed);
        assert_eq!(harness.banner(), None);
    }

    #[test]
    fn only_a_published_port_is_opened() {
        let mut harness = Harness::running();
        let open = |port: &str| UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: format!("open:{port}"),
        };
        harness.event(open("8080"));
        assert!(harness.updates().is_empty());
        let mut web = container("web", None, true);
        web.ports = Some(vec![docklet_engine::models::PortSummary {
            ip: None,
            private_port: 80,
            public_port: Some(8080),
            typ: None,
        }]);
        harness.message(Message::List {
            kind: ResourceKind::Container,
            result: Ok(List::Containers(vec![web])),
        });
        harness.updates();
        harness.event(open("8080"));
        assert!(matches!(harness.updates()[..], [UiUpdate::OpenPort(8080)]));
        harness.event(open("22"));
        assert!(harness.updates().is_empty());
        // The row's Open button opens the first port it publishes.
        harness.event(UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "open".to_string(),
        });
        assert!(matches!(harness.updates()[..], [UiUpdate::OpenPort(8080)]));
    }

    #[test]
    fn the_windows_own_start_shows_at_once_and_its_failure_is_said() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        harness.message(status(EngineState::Stopped));
        harness.calls();
        harness.event(UiEvent::Button {
            key: "engine-start".to_string(),
        });
        assert_eq!(harness.calls(), ["start"]);
        let screen = harness.screen();
        assert_eq!(screen.title, "Starting the engine…");
        assert!(!screen.primary.unwrap().enabled);
        // A second request while the first runs is not even made.
        harness.event(UiEvent::Tray(TrayCommand::StopEngine));
        assert_eq!(harness.calls(), [] as [&str; 0]);

        *harness.engine.acting.borrow_mut() = false;
        harness.message(Message::EngineRequestDone {
            transition: Transition::Starting,
            error: Some("dockerd did not answer".to_string()),
        });
        assert_eq!(harness.screen().title, "The engine is stopped");
        assert_eq!(harness.banner().unwrap().text, "dockerd did not answer");
        assert_eq!(harness.notice(), None);
    }

    #[test]
    fn a_stop_takes_the_lists_down_before_the_engine_is_gone() {
        let mut harness = Harness::running();
        harness.event(UiEvent::Button {
            key: "engine-stop".to_string(),
        });
        assert_eq!(harness.calls(), ["stop"]);
        assert_eq!(harness.screen().title, "Stopping the engine…");
        // A poll from before the stop still says `Running`.
        harness.message(status(EngineState::Running));
        assert_eq!(harness.calls(), [] as [&str; 0]);
        assert_eq!(harness.screen().title, "Stopping the engine…");
        *harness.engine.acting.borrow_mut() = false;
        harness.message(Message::EngineRequestDone {
            transition: Transition::Stopping,
            error: None,
        });
        harness.message(status(EngineState::Stopped));
        assert_eq!(harness.screen().title, "The engine is stopped");
        assert!(harness.controller.list(ResourceKind::Container).is_empty());
    }

    #[test]
    fn a_start_from_the_tray_without_a_window_reports_in_a_box() {
        let mut harness = harness();
        harness.event(UiEvent::Tray(TrayCommand::StartEngine));
        assert_eq!(harness.calls(), ["start"]);
        *harness.engine.acting.borrow_mut() = false;
        harness.message(Message::EngineRequestDone {
            transition: Transition::Starting,
            error: Some("the pipe proxy did not start".to_string()),
        });
        let notice = harness.notice().unwrap();
        assert_eq!(notice.message, "the pipe proxy did not start");
        assert_eq!(notice.tone, view::Tone::Error);
        // The poll that follows is not a reason to load anything.
        harness.message(status(EngineState::Running));
        assert_eq!(harness.calls(), [] as [&str; 0]);
    }

    #[test]
    fn the_tray_offers_what_applies_only_while_somebody_looks() {
        let mut harness = harness();
        let tray = |harness: &Harness| -> view::TrayState {
            let known = harness
                .controller
                .state()
                .filter(|_| harness.controller.fresh);
            present::tray_view(known, harness.engine.is_acting())
        };
        assert!(tray(&harness).can_start && tray(&harness).can_stop);
        harness.event(UiEvent::WindowShown);
        harness.message(status(EngineState::Stopped));
        assert!(tray(&harness).can_start && !tray(&harness).can_stop);
        assert_eq!(tray(&harness).tooltip, "Docklet: Stopped");
        harness.event(UiEvent::WindowHidden);
        assert_eq!(tray(&harness).tooltip, "Docklet");
        assert!(tray(&harness).can_stop);
    }

    #[test]
    fn an_install_takes_the_picked_base_and_shows_its_steps() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        harness.message(status(EngineState::NotInstalled));
        let choice = harness.screen().choice.unwrap();
        assert_eq!(choice.options, ["debian-13", "alpine"]);
        // No engine, no meters.
        assert_eq!(harness.calls(), ["window Visible"]);
        harness.event(UiEvent::ChoiceChanged { index: 1 });
        harness.event(UiEvent::Button {
            key: "install".to_string(),
        });
        assert_eq!(harness.calls(), ["install alpine"]);
        // The button is disabled, and a second click would do nothing.
        harness.event(UiEvent::Button {
            key: "install".to_string(),
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
        harness.message(Message::InstallProgress("Downloading".to_string()));
        harness.message(Message::InstallProgress("Importing".to_string()));
        let screen = harness.screen();
        assert_eq!(screen.detail.as_deref(), Some("Downloading\nImporting"));
        assert_eq!(screen.progress, view::Progress::Busy);

        harness.message(Message::EngineRequestDone {
            transition: Transition::Installing,
            error: Some("no network".to_string()),
        });
        let screen = harness.screen();
        assert_eq!(screen.primary.unwrap().label, "Install");
        // The steps stay to be read next to the error.
        assert!(screen.detail.is_some());
        assert_eq!(harness.banner().unwrap().text, "no network");
    }

    #[test]
    fn the_wsl_screen_copies_its_command() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        harness.message(status(EngineState::WslMissing));
        harness.updates();
        harness.event(UiEvent::Button {
            key: "copy-command".to_string(),
        });
        assert!(matches!(
            &harness.updates()[..],
            [UiUpdate::Copy(command)] if command == "wsl --install --no-distribution"
        ));
        harness.calls();
        harness.event(UiEvent::Button {
            key: "refresh".to_string(),
        });
        assert_eq!(harness.calls(), ["refresh"]);
    }

    #[test]
    fn a_poll_that_fails_before_anything_is_known_shows_as_a_screen() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        harness.message(Message::Status(Err("wsl.exe did not answer".to_string())));
        let screen = harness.screen();
        assert_eq!(screen.title, "The engine hit a problem");
        assert_eq!(screen.detail.as_deref(), Some("wsl.exe did not answer"));
        // Later the state is kept and the failure is a line above it.
        harness.message(status(EngineState::Stopped));
        harness.message(Message::Status(Err("busy".to_string())));
        assert_eq!(harness.screen().title, "The engine is stopped");
        assert_eq!(
            harness.banner().unwrap().text,
            "The engine state could not be read: busy"
        );
    }

    #[test]
    fn a_pull_shows_its_progress_and_empties_the_box_when_it_worked() {
        let mut harness = Harness::running();
        harness.event(UiEvent::TabChanged { tab: 1 });
        harness.calls();
        let entry = Harness::entry;
        harness.event(UiEvent::Submitted {
            tab: 1,
            text: "   ".to_string(),
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
        harness.event(UiEvent::Submitted {
            tab: 1,
            text: " nginx:1.29 ".to_string(),
        });
        assert_eq!(harness.calls(), ["pull  nginx:1.29 "]);
        assert_eq!(entry(&harness).status, "Pulling nginx:1.29…");
        assert!(!entry(&harness).enabled);
        // One pull at a time.
        harness.event(UiEvent::Submitted {
            tab: 1,
            text: "redis".to_string(),
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
        harness.message(Message::PullProgress("a3ed Downloading 45%".to_string()));
        assert_eq!(entry(&harness).status, "a3ed Downloading 45%");
        harness.updates();
        harness.message(Message::PullDone {
            reference: "nginx:1.29".to_string(),
            error: None,
        });
        let done = entry(&harness);
        assert_eq!(done.text.as_deref(), Some(""));
        assert!(done.enabled && done.status.is_empty());
        // Sent once: the box is not emptied again by what follows.
        let entries = |updates: Vec<UiUpdate>| -> usize {
            let sent = updates.iter();
            sent.filter(|update| matches!(update, UiUpdate::Entry { .. }))
                .count()
        };
        assert_eq!(entries(harness.updates()), 1);
        harness.message(status(EngineState::Running));
        assert_eq!(entries(harness.updates()), 0);
        assert_eq!(harness.banner().unwrap().text, "Pulled nginx:1.29.");

        harness.event(UiEvent::Submitted {
            tab: 1,
            text: "nope:".to_string(),
        });
        harness.message(Message::PullDone {
            reference: "nope:".to_string(),
            error: Some("invalid image reference `nope:`".to_string()),
        });
        assert_eq!(entry(&harness).text, None);
        assert_eq!(
            harness.banner().unwrap().text,
            "invalid image reference `nope:`"
        );
    }

    #[test]
    fn a_log_is_followed_while_it_shows_and_not_a_moment_longer() {
        let mut harness = Harness::running();
        harness.event(UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "logs".to_string(),
        });
        assert_eq!(harness.calls(), ["follow id-web"]);
        let pane = Harness::pane;
        assert_eq!(pane(&harness).title, "web");
        // The view reads what the engine's thread writes.
        harness.engine.logs.borrow()[0].append(["hello".to_string()]);
        assert_eq!(pane(&harness).source.bounds(), (0, 1));

        harness.message(Message::Log {
            token: 1,
            message: LogMessage::Ended { error: None },
        });
        assert!(harness.updates().iter().any(|update| matches!(
            update,
            UiUpdate::LogNote(note) if note == "log ended"
        )));
        // The end of some other log is not this one's.
        harness.message(Message::Log {
            token: 9,
            message: LogMessage::Ended { error: None },
        });
        assert!(harness.updates().is_empty());

        harness.event(UiEvent::LogClosed);
        assert_eq!(harness.calls(), ["unfollow 1"]);
        harness.event(UiEvent::LogClosed);
        assert_eq!(harness.calls(), [] as [&str; 0]);
    }

    #[test]
    fn a_log_is_let_go_while_the_window_is_minimised_and_taken_up_again() {
        let mut harness = Harness::running();
        harness.event(UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "logs".to_string(),
        });
        harness.calls();
        harness.event(UiEvent::WindowHidden);
        // `Gone` ends the log with everything else.
        assert_eq!(harness.calls(), ["usage false", "window Gone"]);
        harness.event(UiEvent::WindowShown);
        harness.updates();
        harness.message(status(EngineState::Running));
        assert_eq!(
            harness.calls(),
            [
                "window Visible",
                "usage true",
                "show container",
                "follow id-web"
            ]
        );
        assert!(
            harness
                .updates()
                .iter()
                .any(|update| matches!(update, UiUpdate::ShowLog(_)))
        );
        // A fresh buffer: the last lines come again and are not doubled.
        assert_eq!(harness.engine.logs.borrow().len(), 2);
    }

    #[test]
    fn an_engine_that_stops_takes_its_log_and_its_questions_along() {
        let mut harness = Harness::running();
        harness.event(UiEvent::RowAction {
            tab: 0,
            id: "id-web".to_string(),
            action: "logs".to_string(),
        });
        harness.event(UiEvent::RemoveRequested {
            tab: 0,
            id: Some("id-db".to_string()),
        });
        let confirm = harness.confirm();
        harness.calls();
        harness.updates();
        harness.message(status(EngineState::Sleeping));
        assert_eq!(harness.calls(), ["unfollow 1"]);
        assert!(
            harness
                .updates()
                .iter()
                .any(|update| matches!(update, UiUpdate::CloseLog))
        );
        // The answer to a question from before removes nothing now.
        harness.event(UiEvent::Confirmed {
            token: confirm.token,
            accepted: true,
        });
        assert_eq!(harness.calls(), [] as [&str; 0]);
    }

    #[test]
    fn usage_fills_the_strip_and_the_columns() {
        let mut harness = Harness::running();
        harness.message(Message::HostUsage(HostReading::Failed(
            "no access".to_string(),
        )));
        let strip = harness.meters();
        assert_eq!(strip.len(), 5);
        // Not known is text, never an error.
        assert_eq!(strip[0].text, "Unknown");
        assert_eq!(strip[4].text, "Not measured");
        assert_eq!(harness.banner(), None);

        harness.message(Message::ContainerUsage(Some(vec![usage(
            "web",
            Some(12.5),
            52_400_000,
        )])));
        let table = harness.table(0);
        let web = table
            .lines
            .iter()
            .find_map(|line| match line {
                view::Line::Row(row) if row.id == "id-web" => Some(row.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(web.cells[4..6], ["12.5%", "52.4 MB"]);

        harness.event(UiEvent::UsageClicked);
        assert!(
            harness
                .notice()
                .unwrap()
                .message
                .contains("Memory: Unknown. no access")
        );
    }

    #[test]
    fn the_same_figures_again_send_nothing() {
        let mut harness = Harness::running();
        let reading = || Message::HostUsage(HostReading::Failed("no access".to_string()));
        harness.message(reading());
        harness.updates();
        harness.message(reading());
        assert!(harness.updates().is_empty());
    }

    #[test]
    fn disk_usage_is_measured_when_asked_and_fills_the_volume_sizes() {
        let mut harness = Harness::running();
        harness.event(UiEvent::TabChanged { tab: 2 });
        harness.message(Message::List {
            kind: ResourceKind::Volume,
            result: Ok(List::Volumes(vec![volume("data")])),
        });
        harness.calls();
        let size = |harness: &Harness| -> String {
            match &harness.table(2).lines[0] {
                view::Line::Row(row) => row.cells[2].clone(),
                view::Line::Group { .. } => panic!("a group"),
            }
        };
        assert_eq!(size(&harness), "—");
        let measure = UiEvent::Command {
            tab: 2,
            key: "disk-usage".to_string(),
        };
        harness.event(measure.clone());
        // Asked once, however often it is clicked while it is worked out.
        harness.event(measure);
        assert_eq!(harness.calls(), ["disk"]);
        let disk = DiskUsage {
            volumes: vec![docklet_engine::VolumeDisk {
                name: "data".to_string(),
                driver: "local".to_string(),
                size: Some(2_340_000_000),
                ref_count: Some(0),
            }],
            volume_totals: docklet_engine::DiskTotals {
                count: 1,
                active: 0,
                size: 2_340_000_000,
                reclaimable: 2_340_000_000,
            },
            ..Default::default()
        };
        harness.message(Message::DiskUsage(Ok(disk)));
        assert_eq!(size(&harness), "2.3 GB");
        assert_eq!(
            harness.table(2).summary,
            "1 volume · 2.3 GB, 2.3 GB reclaimable"
        );
        assert_eq!(
            harness.notice().unwrap().title,
            "Docker keeps 2.3 GB on disk"
        );

        // After a prune the figures are from before: they go.
        harness.message(Message::Pruned {
            target: PruneTarget::UnusedVolumes,
            result: Ok(PruneReport::default()),
        });
        assert_eq!(harness.table(2).summary, "1 volume");
    }

    #[test]
    fn the_mirrored_networking_warning_shows_until_it_is_dismissed() {
        let mut harness = harness();
        harness.event(UiEvent::WindowShown);
        harness.message(Message::Status(Ok(EngineStatus {
            state: EngineState::Stopped,
            bases: Vec::new(),
            mirrored_networking: true,
        })));
        let banner = harness.banner().unwrap();
        assert_eq!(banner.tone, view::Tone::Warning);
        assert!(banner.text.contains("networkingMode=mirrored"));
        harness.event(UiEvent::BannerDismissed);
        assert_eq!(harness.banner(), None);
    }

    #[test]
    fn the_polls_of_an_engine_that_stays_stopped_send_nothing() {
        let mut harness = Harness::running();
        harness.message(status(EngineState::Stopped));
        // No table goes to a message screen, and the lists are empty again.
        assert!(
            !harness
                .updates()
                .iter()
                .any(|update| matches!(update, UiUpdate::Table { .. }))
        );
        assert!(harness.controller.list(ResourceKind::Container).is_empty());
        harness.calls();
        for _ in 0..3 {
            harness.message(status(EngineState::Stopped));
        }
        assert!(harness.updates().is_empty());
        assert_eq!(harness.calls(), [] as [&str; 0]);
    }

    #[test]
    fn what_did_not_change_is_not_sent_again() {
        let mut harness = Harness::running();
        harness.message(status(EngineState::Running));
        harness.event(UiEvent::Wake);
        harness.event(UiEvent::LogOpened);
        assert!(harness.updates().is_empty());
    }
}
