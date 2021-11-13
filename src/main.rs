use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::SeqCst},
        Arc, Mutex,
    },
};

use anyhow::{bail, ensure};
use chrono::{serde::ts_milliseconds_option, DateTime, Utc};
use copypasta::ClipboardProvider;
use eframe::{
    egui::{self, vec2, Button, DragValue, Grid, Layout, ProgressBar, Slider, TextEdit, Ui},
    epi,
};
use num_format::{Locale, ToFormattedString};
use pgp::Deserializable;
use progress_streams::ProgressReader;
use rayon::prelude::*;
use regex::Regex;
use rfd::{MessageButtons, MessageDialog, MessageLevel};
use serde::{Deserialize, Serialize};
use triple_accel::levenshtein::{self, EditCosts};

struct Username {
    id: String,
    name: String,
}

struct Match {
    id: String,
    name: String,
    enabled: bool,
    created_at: Option<DateTime<Utc>>,
    seen_at: Option<DateTime<Utc>>,
    games: u32,
    k: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiUser {
    id: String,
    #[serde(default, with = "ts_milliseconds_option")]
    created_at: Option<DateTime<Utc>>,
    #[serde(default, with = "ts_milliseconds_option")]
    seen_at: Option<DateTime<Utc>>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    perfs: ApiPerfs,
}

#[derive(Default, Deserialize)]
#[serde(default)]
#[allow(non_snake_case)]
struct ApiPerfs {
    chess960: Option<ApiPerf>,
    atomic: Option<ApiPerf>,
    racingKings: Option<ApiPerf>,
    ultraBullet: Option<ApiPerf>,
    blitz: Option<ApiPerf>,
    kingOfTheHill: Option<ApiPerf>,
    bullet: Option<ApiPerf>,
    correspondence: Option<ApiPerf>,
    horde: Option<ApiPerf>,
    classical: Option<ApiPerf>,
    rapid: Option<ApiPerf>,
}

impl ApiPerfs {
    fn sum_games(self) -> u32 {
        self.chess960.map(|p| p.games).unwrap_or(0)
            + self.atomic.map(|p| p.games).unwrap_or(0)
            + self.racingKings.map(|p| p.games).unwrap_or(0)
            + self.ultraBullet.map(|p| p.games).unwrap_or(0)
            + self.blitz.map(|p| p.games).unwrap_or(0)
            + self.kingOfTheHill.map(|p| p.games).unwrap_or(0)
            + self.bullet.map(|p| p.games).unwrap_or(0)
            + self.correspondence.map(|p| p.games).unwrap_or(0)
            + self.horde.map(|p| p.games).unwrap_or(0)
            + self.classical.map(|p| p.games).unwrap_or(0)
            + self.rapid.map(|p| p.games).unwrap_or(0)
    }
}

#[derive(Deserialize)]
struct ApiPerf {
    #[serde(default)]
    games: u32,
}

fn main() {
    let app = App::default();
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(Box::new(app), native_options);
}

#[derive(Clone)]
#[allow(clippy::type_complexity)]
struct LoadingState {
    progress: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<Vec<Username>, String>>>>,
}

impl LoadingState {
    fn new() -> Self {
        Self {
            progress: Arc::new(AtomicU32::new(0.0_f32 as u32)),
            done: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Clone, Default)]
struct LoadedState {
    pattern: String,
    users: Arc<Vec<Username>>,
    results: Arc<Mutex<Vec<Match>>>,
    processing: Arc<AtomicBool>,
    page: usize,
    progress: Arc<AtomicUsize>,
}

enum State {
    PickFile,
    AskPassword(PathBuf, String),
    Loading(LoadingState),
    Loaded(LoadedState),
}

impl State {
    fn loaded(users: Vec<Username>) -> Self {
        Self::Loaded(LoadedState {
            pattern: String::new(),
            users: Arc::new(users),
            results: Default::default(),
            processing: Default::default(),
            page: 0,
            progress: Arc::new(AtomicUsize::new(1)),
        })
    }
}

impl Default for State {
    fn default() -> Self {
        Self::PickFile
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Plain,
    NumberReplacements,
    Levenshtein,
    RegEx,
}

#[derive(Clone)]
enum Searcher {
    Plain(String),
    Regex(Regex),
    Levenshtein(String, LevenshteinSettings),
}

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

impl Default for SearchMode {
    fn default() -> Self {
        Self::Plain
    }
}

#[derive(Deserialize, Serialize, Clone, Copy)]
struct LevenshteinSettings {
    max_k: u32,
    mismatch_cost: u8,
    gap_cost: u8,
    swap_cost: u8,
}

impl LevenshteinSettings {
    fn edit_costs(&self) -> EditCosts {
        levenshtein::EditCosts::new(self.mismatch_cost, self.gap_cost, 0, Some(self.swap_cost))
    }
}

impl Default for LevenshteinSettings {
    fn default() -> Self {
        Self {
            max_k: 3,
            mismatch_cost: 1,
            gap_cost: 1,
            swap_cost: 1,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct App {
    #[serde(default = "default_page_size")]
    page_size: usize,
    password: Option<String>,
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
                    self.state =
                        State::AskPassword(path, self.password.clone().unwrap_or_default());
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

    fn load_encrypted(&mut self, path: PathBuf, password: String) {
        let s = LoadingState::new();
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
                    .decrypt_with_password(|| password)?
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
                        show_error(&error.to_string());
                        return;
                    }
                }
            }
            SearchMode::Levenshtein => Searcher::Levenshtein(pattern, lev),
            SearchMode::RegEx => match Regex::new(&pattern) {
                Ok(regex) => Searcher::Regex(regex),
                Err(error) => {
                    show_error(&error.to_string());
                    return;
                }
            },
        };

        s.processing.store(true, SeqCst);
        s.progress.store(0, SeqCst);

        std::thread::spawn(move || {
            s.users
                .par_chunks(100_000)
                .for_each_with(searcher, |searcher, users| {
                    let mut curr = Vec::new();
                    for (i, user) in users.iter().enumerate() {
                        if let Some(k) = searcher.matches(&user.id) {
                            curr.push(Match {
                                id: user.id.clone(),
                                name: user.name.clone(),
                                enabled: true,
                                created_at: None,
                                seen_at: None,
                                games: 0,
                                k,
                            })
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
            password: None,
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
            AskPassword(path, pwd) => {
                let mut decrypt = false;
                ui.vertical_centered_justified(|ui| {
                    ui.label("Password:");
                    ui.add(TextEdit::singleline(pwd).password(true));
                    decrypt = ui.button("Decrypt").clicked();
                });
                if decrypt {
                    self.password = Some(pwd.clone());
                    let path = path.clone();
                    let pwd = pwd.clone();
                    self.load_encrypted(path, pwd);
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
                                show_error(&msg);
                                self.password = None;
                                self.state = PickFile;
                            }
                            None => (),
                        }
                    }
                });
            }
            Loaded(s) => {
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

                ui.add_enabled_ui(!s.processing.load(SeqCst), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Username: ");
                        do_search = ui.text_edit_singleline(&mut s.pattern).lost_focus();
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
                                *results = coll
                                    .iter()
                                    .map(|n| Match {
                                        id: n.to_ascii_lowercase(),
                                        name: n.clone(),
                                        enabled: true,
                                        created_at: None,
                                        seen_at: None,
                                        games: 0,
                                        k: 0,
                                    })
                                    .collect();
                            }
                            if ui.button("Clear").clicked() {
                                coll.clear();
                            }
                            ui.add_space(20.0);
                        }
                        let hint = "Fetch additional information about found users from Lichess";
                        do_update = ui
                            .add_enabled(
                                !s.processing.load(SeqCst),
                                Button::new("Fetch additional info"),
                            )
                            .on_hover_text(hint)
                            .clicked();
                    })
                });

                if do_search {
                    s.page = 0;
                    results.clear();
                    App::do_search(s.clone(), self.search_mode, self.levenshtein_settings);
                }

                if do_update {
                    s.processing.store(true, SeqCst);
                    let max = results.len().min(5 * 300);
                    let progress_step = results.len() / ((max + 299) / 300);
                    let names = results[..max]
                        .iter()
                        .map(|u| &u.name)
                        .cloned()
                        .collect::<Vec<String>>();
                    let progress = s.progress.clone();
                    let processing = s.processing.clone();
                    let results = s.results.clone();
                    std::thread::spawn(move || {
                        progress.store(0, SeqCst);
                        let mut api_users = HashMap::new();
                        for (i, group) in names.chunks(300).enumerate() {
                            match ureq::post("https://lichess.org/api/users")
                                .send_string(&group.join(","))
                                .map_err(|e| e.to_string())
                                .and_then(|r| {
                                    r.into_json::<Vec<ApiUser>>().map_err(|e| e.to_string())
                                }) {
                                Ok(users) => users.into_iter().for_each(|u| {
                                    api_users.insert(u.id.clone(), u);
                                }),
                                Err(msg) => {
                                    show_error(&msg);
                                    break;
                                }
                            }
                            progress.store((i + 1) * progress_step, SeqCst);
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

                ui.separator();

                if s.processing.load(SeqCst) {
                    ctx.request_repaint();
                    ui.add(
                        ProgressBar::new(s.progress.load(SeqCst) as f32 / s.users.len() as f32)
                            .show_percentage(),
                    );
                } else {
                    ui.add(ProgressBar::new(1.0).text("Done"));
                }

                ui.separator();

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

                            for user in &results[(s.page * self.page_size)
                                ..((s.page + 1) * self.page_size).min(results.len())]
                            {
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
    match copypasta::ClipboardContext::new() {
        Ok(mut ctx) => match ctx.set_contents(s) {
            Ok(()) => (),
            Err(error) => show_error(&error.to_string()),
        },
        Err(error) => show_error(&error.to_string()),
    }
}

fn show_error(msg: &str) {
    MessageDialog::new()
        .set_title("Error")
        .set_buttons(MessageButtons::Ok)
        .set_description(&format!("Error: {}", msg))
        .set_level(MessageLevel::Error)
        .show();
}
