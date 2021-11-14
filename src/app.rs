use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    sync::atomic::Ordering::SeqCst,
    time::Duration,
};

use anyhow::{bail, ensure};
use chrono::Utc;
use copypasta::ClipboardProvider;
use eframe::{
    egui::{self, vec2, Button, DragValue, Grid, Key, Layout, ProgressBar, Slider, TextEdit, Ui},
    epi,
};
use num_format::{Locale, ToFormattedString};
use pgp::Deserializable;
use progress_streams::ProgressReader;
use rayon::prelude::*;
use regex::Regex;
use rfd::{MessageButtons, MessageDialog, MessageLevel};
use serde::{Deserialize, Serialize};
use triple_accel::levenshtein::{self};

use crate::api;
use crate::model::*;

const MAX_CLOSE: usize = 250;

impl Searcher {
    fn matches(&self, username: &str) -> Option<u32> {
        use Searcher::*;
        match self {
            Plain(pattern) => username.contains(pattern).then(|| 0),
            Regex(regex) => regex.is_match(username).then(|| 0),
            Levenshtein(pattern, ..) if username.len() < pattern.len() => None,
            Levenshtein(pattern, lev) => levenshtein::levenshtein_search_simd_with_opts(
                pattern.as_bytes(),
                username.as_bytes(),
                lev.max_k,
                // ((pattern.len() >> 1) as u32) + ((pattern.len() as u32) & 1),
                triple_accel::SearchType::Best,
                lev.edit_costs(),
                // levenshtein::RDAMERAU_COSTS,
                false,
            )
            .next()
            .map(|m| m.k),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct App {
    #[serde(default = "default_page_size")]
    page_size: usize,
    password: String,
    api_key: String,
    search_mode: SearchMode,
    levenshtein_settings: LevenshteinSettings,
    saved_borderline: HashSet<String>,
    saved_obvious: HashSet<String>,
    #[serde(skip)]
    state: State,
}

impl App {
    fn render_pick_file(&mut self, ui: &mut Ui) {
        if ui.button("Load Data").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Usernames", &["txt", "txt.gz", "txt.gpg", "txt.gz.gpg"])
                .pick_file()
            {
                let ext = path.extension().unwrap_or_default();
                if ext == "gpg" {
                    self.state = State::AskPassword(path);
                } else {
                    self.load_plain(path);
                }
            }
        }
    }

    fn do_read(reader: impl BufRead) -> Result<Vec<Username>, std::io::Error> {
        let mut result = Vec::new();
        for line in reader.lines() {
            let name = line?;
            if !name.is_empty() {
                result.push(Username {
                    id: name.to_ascii_lowercase(),
                    name,
                });
            }
        }
        Ok(result)
    }

    fn load_plain(&mut self, path: PathBuf) {
        let s = LoadingState::new();
        self.state = State::Loading(s.clone());
        std::thread::spawn(move || {
            let load_file = |path: PathBuf| -> Result<Vec<Username>, std::io::Error> {
                let compressed = path.extension().filter(|e| *e == "gz").is_some();
                let file = std::fs::File::open(path)?;
                let size = file.metadata()?.len() as f32;
                let mut read = 0;
                let reader = ProgressReader::new(file, |progress: usize| {
                    read += progress;
                    let progress = (read as f32) / size;
                    s.progress.store(progress.to_bits(), SeqCst);
                });
                let reader = BufReader::new(reader);
                if compressed {
                    let reader = flate2::bufread::GzDecoder::new(reader);
                    let reader = BufReader::new(reader);
                    Self::do_read(reader)
                } else {
                    Self::do_read(reader)
                }
            };

            *s.result.lock().unwrap() = Some(load_file(path).map_err(|e| e.to_string()));
            s.done.store(true, SeqCst);
        });
    }

    fn load_encrypted(&mut self, path: PathBuf) {
        let s = LoadingState::new();
        let pwd = self.password.clone();
        self.state = State::Loading(s.clone());
        std::thread::spawn(move || {
            let load_file = |path: PathBuf| -> anyhow::Result<Vec<Username>> {
                let compressed = path.as_os_str().to_str().unwrap().contains(".gz.");
                let file = std::fs::File::open(path)?;
                let size = file.metadata()?.len() as f32;
                let mut read = 0;
                let reader = ProgressReader::new(file, |progress: usize| {
                    read += progress;
                    let divider = if compressed { 4.0 } else { 2.0 };
                    let progress = (read as f32) / size / divider;
                    s.progress.store(progress.to_bits(), SeqCst);
                });
                let reader = BufReader::new(reader);
                let msg = pgp::Message::from_bytes(reader)?;
                let msgs = msg
                    .decrypt_with_password(|| pwd)?
                    .collect::<Result<Vec<_>, _>>()?;
                ensure!(
                    msgs.len() == 1,
                    "Expected one PGP message, got {}",
                    msgs.len()
                );
                let msg = msgs.into_iter().next().unwrap().decompress()?;

                if let Some(literal) = msg.get_literal() {
                    let mut data = literal.data();
                    let mut v = Vec::new();
                    if compressed {
                        let mut read = 0;
                        let reader = ProgressReader::new(data, |progress: usize| {
                            read += progress;
                            let progress = (read as f32) / data.len() as f32 / 4.0 + 0.25;
                            s.progress.store(progress.to_bits(), SeqCst);
                        });
                        let reader = BufReader::new(reader);
                        flate2::bufread::GzDecoder::new(reader).read_to_end(&mut v)?;
                        data = &v;
                    }
                    let mut read = 0;
                    let mut result = Vec::new();
                    let size = data.len() as f32;
                    for (i, line) in data.lines().enumerate() {
                        let line = line?;
                        let name = line.to_string();
                        if !name.is_empty() {
                            result.push(Username {
                                id: name.to_ascii_lowercase(),
                                name,
                            });
                        }
                        read += line.len();
                        if i % 100_000 == 0 {
                            let progress = (read as f32) / size / 2.0 + 0.5;
                            s.progress.store(progress.to_bits(), SeqCst);
                        }
                    }
                    Ok(result)
                } else {
                    bail!("Failed to decrypt message")
                }
            };

            *s.result.lock().unwrap() = Some(load_file(path).map_err(|e| e.to_string()));
            s.done.store(true, SeqCst);
        });
    }

    fn do_search(s: LoadedState, mode: SearchMode, lev: LevenshteinSettings) {
        if s.pattern.len() < 3 {
            return;
        }

        let pattern = s.pattern.to_ascii_lowercase();
        let searcher = match mode {
            SearchMode::Plain => Searcher::Plain(pattern),
            SearchMode::NumberReplacements => {
                let pattern = pattern
                    .replace("a", "[a4]")
                    .replace("e", "[e3]")
                    .replace("g", "[gq9]")
                    .replace("i", "[il1]")
                    .replace("l", "[il1]")
                    .replace("o", "[o0]")
                    .replace("s", "[s5]")
                    .replace("u", "[uv]")
                    .replace("z", "[z2]");
                match Regex::new(&pattern) {
                    Ok(regex) => Searcher::Regex(regex),
                    Err(error) => {
                        show_error(error);
                        return;
                    }
                }
            }
            SearchMode::Levenshtein => Searcher::Levenshtein(pattern, lev),
            SearchMode::RegEx => match Regex::new(&pattern) {
                Ok(regex) => Searcher::Regex(regex),
                Err(error) => {
                    show_error(error);
                    return;
                }
            },
        };

        s.processing.store(true, SeqCst);
        s.progress.store(0, SeqCst);
        s.progress_max.store(s.users.len().max(1), SeqCst);

        std::thread::spawn(move || {
            s.users
                .par_chunks(100_000)
                .for_each_with(searcher, |searcher, users| {
                    let mut curr = Vec::new();
                    for (i, user) in users.iter().enumerate() {
                        if let Some(k) = searcher.matches(&user.id) {
                            curr.push(Match::new(user, k));
                        }
                        if i & 0xfff == 0 {
                            s.progress.fetch_add(0xfff, SeqCst);
                        }
                    }
                    s.results.lock().unwrap().append(&mut curr);
                });
            if mode == SearchMode::Levenshtein {
                s.results.lock().unwrap().sort_unstable_by_key(|m| m.k);
            }
            s.processing.store(false, SeqCst);
        });
    }
}

fn default_page_size() -> usize {
    20
}

impl Default for App {
    fn default() -> Self {
        Self {
            page_size: default_page_size(),
            password: Default::default(),
            api_key: Default::default(),
            levenshtein_settings: Default::default(),
            search_mode: Default::default(),
            saved_borderline: Default::default(),
            saved_obvious: Default::default(),
            state: Default::default(),
        }
    }
}

impl epi::App for App {
    fn name(&self) -> &str {
        "Lichess User Search"
    }

    fn setup(
        &mut self,
        _ctx: &egui::CtxRef,
        _frame: &mut epi::Frame<'_>,
        _storage: Option<&dyn epi::Storage>,
    ) {
        if let Some(storage) = _storage {
            *self = epi::get_value(storage, epi::APP_KEY).unwrap_or_default()
        }
    }

    fn save(&mut self, storage: &mut dyn epi::Storage) {
        epi::set_value(storage, epi::APP_KEY, self);
    }

    fn update(&mut self, ctx: &egui::CtxRef, _frame: &mut epi::Frame<'_>) {
        use State::*;

        egui::CentralPanel::default().show(ctx, |ui| match &mut self.state {
            PickFile => {
                ui.centered_and_justified(|ui| self.render_pick_file(ui));
            }
            AskPassword(path) => {
                let mut decrypt = false;
                ui.vertical_centered(|ui| {
                    ui.add_space(200.0);
                    ui.label("Password:");
                    decrypt |= ui
                        .add(TextEdit::singleline(&mut self.password).password(true))
                        .lost_focus()
                        && ui.input().key_pressed(Key::Enter);
                    decrypt |= ui.button("Decrypt").clicked();
                });
                if decrypt {
                    let path = path.clone();
                    self.load_encrypted(path);
                }
            }
            Loading(s) => {
                ctx.request_repaint();
                let s = s.clone();
                ui.vertical_centered_justified(|ui| {
                    ui.add(
                        ProgressBar::new(f32::from_bits(s.progress.load(SeqCst))).show_percentage(),
                    );

                    if s.done.load(SeqCst) {
                        match s.result.lock().unwrap().take() {
                            Some(Ok(data)) => {
                                self.state = State::loaded(data);
                            }
                            Some(Err(msg)) => {
                                show_error(msg);
                                self.state = PickFile;
                            }
                            None => (),
                        }
                    }
                });
            }
            Loaded(s) => {
                // First taskbar (search controls)
                ui.horizontal_wrapped(|ui| {
                    ui.add(Slider::new(&mut self.page_size, 10..=100).text("Results per page"));

                    ui.add_space(20.0);
                    ui.label("Search mode: ");
                    ui.radio_value(&mut self.search_mode, SearchMode::Plain, "Plain")
                        .on_hover_text("Search for the text as is");
                    ui.radio_value(
                        &mut self.search_mode,
                        SearchMode::NumberReplacements,
                        "Similar letters",
                    )
                    .on_hover_text(
                        "Search patterns with similar letters/numbers, e.g. 1 instead of l",
                    );
                    ui.radio_value(
                        &mut self.search_mode,
                        SearchMode::Levenshtein,
                        "Levenshtein",
                    )
                    .on_hover_text("Search similar patterns based on levenshtein distance");
                    ui.radio_value(&mut self.search_mode, SearchMode::RegEx, "RegEx");

                    if let SearchMode::Levenshtein = self.search_mode {
                        ui.add_space(20.0);
                        ui.label("Max distance:");
                        ui.add(
                            DragValue::new(&mut self.levenshtein_settings.max_k)
                                .speed(0.2)
                                .clamp_range(1..=10),
                        );
                        ui.add_space(20.0);
                        ui.label("Cost:");
                        ui.add(
                            DragValue::new(&mut self.levenshtein_settings.mismatch_cost)
                                .speed(0.2)
                                .clamp_range(1..=10),
                        )
                        .on_hover_text("Mismatch cost (cost of an incorrect letter)");
                        ui.add(
                            DragValue::new(&mut self.levenshtein_settings.gap_cost)
                                .speed(0.2)
                                .clamp_range(1..=10),
                        )
                        .on_hover_text("Gap cost (cost of a missing/additional letter)");
                        let max_cost = self
                            .levenshtein_settings
                            .mismatch_cost
                            .min(self.levenshtein_settings.gap_cost)
                            * 2
                            - 1;
                        ui.add(
                            DragValue::new(&mut self.levenshtein_settings.swap_cost)
                                .speed(0.2)
                                .clamp_range(1..=max_cost),
                        )
                        .on_hover_text("Swap cost (cost of swapping two adjacent letters)");
                    }
                });

                ui.separator();

                let results = s.results.clone();
                let mut results = results.lock().unwrap();
                let mut do_search = false;
                let mut do_update = false;
                let mut do_load_clipboard = false;
                let mut do_close = false;

                // Second taskbar (search input + save lists)
                ui.add_enabled_ui(!s.processing.load(SeqCst), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Username: ");
                        do_search |= ui.text_edit_singleline(&mut s.pattern).lost_focus()
                            && ui.input().key_pressed(Key::Enter);
                        do_search |= ui.button("Search").clicked();
                        ui.label(format!(
                            "Matches {}/{:}",
                            results.len(),
                            s.users.len().to_formatted_string(&Locale::en)
                        ));
                        ui.add_space(20.0);
                        for (name, coll) in [
                            ("obvious", &mut self.saved_obvious),
                            ("borderline", &mut self.saved_borderline),
                        ] {
                            if coll.is_empty() {
                                ui.label(format!("No {} names", name));
                            } else {
                                ui.label(format!("{} {} names", coll.len(), name));
                            }
                            if ui.button("Copy").clicked() {
                                copy_to_clipboard(
                                    coll.iter()
                                        .map(|n| format!("/{}", n))
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                );
                            }
                            if ui.button("Show").clicked() {
                                *results = coll.iter().map(From::from).collect();
                            }
                            if ui.button("Clear").clicked() {
                                coll.clear();
                            }
                            ui.add_space(20.0);
                        }
                        do_load_clipboard = ui
                            .button("Load from clipboard")
                            .on_hover_text(
                                "One username per line with leading slash or Lichess URL.\n\
                                 Non-conforming lines and text after the username will be removed.",
                            )
                            .clicked();
                        ui.add_space(20.0);
                        do_update = ui
                            .button("Fetch additional info")
                            .on_hover_text(
                                "Fetch additional information about found users from Lichess",
                            )
                            .clicked();
                        let hint = "Close currently shown accounts via the Lichess API \
                                    (requires Admin API key)";
                        let close_enabled = results.len() < MAX_CLOSE;
                        let close_btn = ui
                            .add_enabled(close_enabled, Button::new("Close accounts"))
                            .on_hover_text(hint)
                            .on_disabled_hover_text(format!(
                                "Closing more than {} accounts at once \
                                 is not allowed for safety reasons",
                                MAX_CLOSE
                            ));
                        let api_key_popup_id = ui.make_persistent_id("api_key_popup");
                        if close_btn.clicked() {
                            ui.memory().toggle_popup(api_key_popup_id);
                        }
                        egui::popup::popup_below_widget(ui, api_key_popup_id, &close_btn, |ui| {
                            ui.set_min_width(200.0);
                            ui.label("Admin API key:");
                            let api_key_edit = ui.text_edit_singleline(&mut self.api_key);
                            api_key_edit.request_focus();
                            do_close |=
                                api_key_edit.lost_focus() && ui.input().key_pressed(Key::Enter);
                            do_close |= ui
                                .add_enabled(
                                    !self.api_key.is_empty(),
                                    Button::new("Close accounts"),
                                )
                                .clicked();
                        });
                    })
                });

                if do_search {
                    s.page = 0;
                    results.clear();
                    App::do_search(s.clone(), self.search_mode, self.levenshtein_settings);
                }

                if do_update {
                    s.processing.store(true, SeqCst);
                    s.progress.store(0, SeqCst);
                    let max = results.len().min(5 * 300);
                    s.progress_max.store(max.max(1), SeqCst);
                    let names = results[..max]
                        .iter()
                        .map(|u| &u.name)
                        .cloned()
                        .collect::<Vec<String>>();
                    let progress = s.progress.clone();
                    let processing = s.processing.clone();
                    let results = s.results.clone();
                    std::thread::spawn(move || {
                        let mut api_users = HashMap::new();
                        for group in names.chunks(300) {
                            match api::fetch_users(group) {
                                Ok(users) => {
                                    api_users.extend(users.into_iter().map(|u| (u.id.clone(), u)))
                                }
                                Err(err) => {
                                    show_error(err);
                                    break;
                                }
                            }
                            progress.fetch_add(1, SeqCst);
                        }
                        if !api_users.is_empty() {
                            let mut results = results.lock().unwrap();
                            for user in results[..max].iter_mut() {
                                if let Some(u) = api_users.remove(&user.id) {
                                    user.created_at = u.created_at;
                                    user.seen_at = u.seen_at;
                                    user.games = u.perfs.sum_games();
                                    user.enabled = !u.disabled;
                                }
                            }
                        }
                        processing.store(false, SeqCst);
                    });
                }

                if do_load_clipboard {
                    if let Some(clip) = get_clipboard() {
                        let regex =
                            Regex::new("^(?:https://lichess.org/@)?/([a-zA-Z0-9_-]{2,30})\\b")
                                .unwrap();
                        *results = clip
                            .lines()
                            .filter_map(|l| regex.captures(l))
                            .filter_map(|c| c.get(1))
                            .map(|m| m.as_str())
                            .map(Match::from)
                            .collect();
                    }
                }

                if do_close
                    && results.len() < MAX_CLOSE
                    && MessageDialog::new()
                        .set_title("Confirm close")
                        .set_description(&format!("Close {} accounts?", results.len()))
                        .set_buttons(MessageButtons::OkCancel)
                        .show()
                {
                    s.processing.store(true, SeqCst);
                    s.progress.store(0, SeqCst);
                    s.progress_max.store(results.len().max(1), SeqCst);
                    let progress = s.progress.clone();
                    let processing = s.processing.clone();
                    let api_key = self.api_key.clone();
                    let names = results
                        .iter()
                        .filter(|u| u.enabled)
                        .map(|u| &u.name)
                        .cloned()
                        .collect::<Vec<String>>();
                    std::thread::spawn(move || {
                        for name in &names {
                            if let Err(error) = api::close_account(name, &api_key) {
                                show_error(error);
                                break;
                            }
                            progress.fetch_add(1, SeqCst);
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        processing.store(false, SeqCst);
                    });
                }

                ui.separator();

                // Progress bar
                if s.processing.load(SeqCst) {
                    ctx.request_repaint();
                    ui.add(
                        ProgressBar::new(
                            s.progress.load(SeqCst) as f32 / s.progress_max.load(SeqCst) as f32,
                        )
                        .show_percentage(),
                    );
                } else {
                    ui.add(ProgressBar::new(1.0).text("Done"));
                }

                ui.separator();

                // Results
                ui.add_enabled_ui(!s.processing.load(SeqCst), |ui| {
                    Grid::new("grid")
                        .striped(true)
                        .min_col_width(200.0)
                        .show(ui, |ui| {
                            ui.strong("");
                            ui.strong("Username");
                            ui.strong("Created");
                            ui.strong("Online");
                            ui.strong("Games");
                            ui.end_row();

                            let now = Utc::now();
                            let timeago = |dt| {
                                let d = now.signed_duration_since(dt);
                                if d.num_days() >= 2 * 365 {
                                    format!("{} years ago", d.num_days() / 365)
                                } else if d.num_days() >= 365 {
                                    "1 year ago".to_string()
                                } else if d.num_days() >= 30 {
                                    format!("{} months ago", d.num_days() / 30)
                                } else {
                                    format!("{} days ago", d.num_days())
                                }
                            };

                            let mut min = s.page * self.page_size;
                            let mut max = ((s.page + 1) * self.page_size).min(results.len());
                            if min > max {
                                min = 0;
                                max = self.page_size.min(results.len());
                                s.page = 0;
                            }

                            for user in &results[min..max] {
                                let obvious = self.saved_obvious.contains(&user.name);
                                let borderline = self.saved_borderline.contains(&user.name);
                                let (mut clicked_obv, mut clicked_border) = (false, false);
                                ui.with_layout(Layout::right_to_left(), |ui| {
                                    clicked_obv = ui
                                        .button(if obvious { "–Obvious" } else { "+Obvious" })
                                        .clicked();
                                    clicked_border = ui
                                        .button(if borderline {
                                            "–Borderline"
                                        } else {
                                            "+Borderline"
                                        })
                                        .clicked();
                                });

                                if clicked_border || (clicked_obv && obvious) {
                                    self.saved_obvious.remove(&user.name);
                                }
                                if clicked_obv || (clicked_border && borderline) {
                                    self.saved_borderline.remove(&user.name);
                                }
                                if clicked_obv && !obvious {
                                    self.saved_obvious.insert(user.name.clone());
                                } else if clicked_border && !borderline {
                                    self.saved_borderline.insert(user.name.clone());
                                }
                                ui.hyperlink_to(
                                    format!(
                                        "{} {}{}",
                                        user.name,
                                        if user.enabled { "" } else { "🔒" },
                                        if obvious || borderline { "⭐" } else { "" }
                                    ),
                                    &format!("https://lichess.org/@/{}", user.id),
                                );
                                ui.label(user.created_at.map(timeago).unwrap_or_default());
                                ui.label(user.seen_at.map(timeago).unwrap_or_default());
                                ui.label(user.games);
                                ui.label(user.k);
                                ui.end_row();
                            }
                        });
                });

                // Page navigation
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add_space(150.0);
                    ui.add_enabled_ui(s.page > 0, |ui| {
                        if ui
                            .add_sized(vec2(100.0, 20.0), Button::new("Prev"))
                            .clicked()
                        {
                            s.page -= 1;
                        }
                    });
                    ui.label(format!(
                        "{} / {}",
                        s.page + 1,
                        (results.len() / self.page_size) + 1
                    ));
                    ui.add_enabled_ui((s.page + 1) * self.page_size < results.len(), |ui| {
                        if ui
                            .add_sized(vec2(100.0, 20.0), Button::new("Next"))
                            .clicked()
                        {
                            s.page += 1;
                        }
                    });
                });

                // Handle scrolling (to move through pages)
                match ctx.input().scroll_delta.y {
                    y if y > 0.0 && s.page > 0 => s.page -= 1,
                    y if y < 0.0 && (s.page + 1) * self.page_size < results.len() => s.page += 1,
                    _ => (),
                }
            }
        });
    }
}

fn copy_to_clipboard(s: String) {
    if let Some(Err(error)) = clipboard().map(|mut ctx| ctx.set_contents(s)) {
        show_error(error);
    }
}

fn get_clipboard() -> Option<String> {
    clipboard().and_then(|mut ctx| match ctx.get_contents() {
        Ok(s) => Some(s),
        Err(error) => {
            show_error(error);
            None
        }
    })
}

fn clipboard() -> Option<copypasta::ClipboardContext> {
    match copypasta::ClipboardContext::new() {
        Ok(ctx) => Some(ctx),
        Err(error) => {
            show_error(error);
            None
        }
    }
}

fn show_error(msg: impl ToString) {
    MessageDialog::new()
        .set_title("Error")
        .set_buttons(MessageButtons::Ok)
        .set_description(&format!("Error: {}", msg.to_string()))
        .set_level(MessageLevel::Error)
        .show();
}
