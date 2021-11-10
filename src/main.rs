use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::SeqCst},
        Arc, Mutex,
    },
};

use chrono::{serde::ts_milliseconds_option, DateTime, Utc};
use copypasta::ClipboardProvider;
use eframe::{
    egui::{self, vec2, Button, Grid, Layout, ProgressBar, Slider, Ui},
    epi,
};
use num_format::{Locale, ToFormattedString};
use progress_streams::ProgressReader;
use regex::Regex;
use rfd::{MessageButtons, MessageDialog, MessageLevel};
use serde::{Deserialize, Serialize};
use triple_accel::levenshtein;

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

// #[derive(Serialize)]
// struct CompactUser<'a> {
//     #[serde(rename = "_id")]
//     id: &'a str,
//     enabled: bool,
//     #[serde(skip_serializing_if = "Option::is_none")]
//     username: Option<&'a str>,
// }

#[derive(Deserialize)]
#[serde(rename = "camelCase")]
struct ApiUser {
    id: String,
    #[serde(with = "ts_milliseconds_option")]
    created_at: Option<DateTime<Utc>>,
    #[serde(with = "ts_milliseconds_option")]
    seen_at: Option<DateTime<Utc>>,
    #[serde(default)]
    disabled: bool,
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
    loaded: Arc<AtomicUsize>,
    to_load: usize,
    progress: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<Vec<Username>, String>>>>,
}

impl LoadingState {
    fn new(to_load: usize) -> Self {
        Self {
            loaded: Arc::new(AtomicUsize::new(0)),
            to_load,
            progress: Arc::new(AtomicU32::new(0.0_f32 as u32)),
            done: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Clone, Default)]
struct LoadedState {
    pattern: String,
    edit_costs: String,
    users: Arc<Vec<Username>>,
    results: Arc<Mutex<Vec<Match>>>,
    processing: Arc<AtomicBool>,
    page: usize,
    progress: Arc<AtomicU32>,
}

enum State {
    PickFile,
    Loading(LoadingState),
    // Dumping(Arc<AtomicBool>, LoadedState),
    Loaded(LoadedState),
}

impl State {
    fn loaded(users: Vec<Username>) -> Self {
        Self::Loaded(LoadedState {
            pattern: String::new(),
            edit_costs: "3,1,1,1".to_string(),
            users: Arc::new(users),
            results: Default::default(),
            processing: Default::default(),
            page: 0,
            progress: Arc::new(AtomicU32::new(1)),
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

enum Searcher {
    Plain(String),
    Regex(Regex),
    Levenshtein(String, levenshtein::EditCosts, u32),
}

impl Searcher {
    fn matches(&self, username: &str) -> Option<u32> {
        use Searcher::*;
        match self {
            Plain(pattern) => username.contains(pattern).then(|| 0),
            Regex(regex) => regex.is_match(username).then(|| 0),
            Levenshtein(pattern, ..) if username.len() < pattern.len() => None,
            Levenshtein(pattern, edit_costs, k) => levenshtein::levenshtein_search_simd_with_opts(
                pattern.as_bytes(),
                username.as_bytes(),
                *k,
                // ((pattern.len() >> 1) as u32) + ((pattern.len() as u32) & 1),
                triple_accel::SearchType::Best,
                *edit_costs,
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

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct App {
    #[serde(default = "default_page_size")]
    page_size: usize,
    search_mode: SearchMode,
    saved_names: Vec<String>,
    #[serde(skip)]
    selected: HashSet<usize>,
    #[serde(skip)]
    state: State,
}

impl App {
    fn render_pick_file(&mut self, ui: &mut Ui) {
        if ui.button("Load Data").clicked() {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter("Text", &["txt"])
                .pick_files()
            {
                let s = LoadingState::new(paths.len());
                self.state = State::Loading(s.clone());
                std::thread::spawn(move || {
                    let mut data = Vec::new();

                    for path in paths {
                        let mut load_file = |path| -> Result<(), String> {
                            let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
                            let size = file.metadata().map_err(|e| e.to_string())?.len() as f32;
                            let mut read = 0;
                            let reader = ProgressReader::new(file, |progress: usize| {
                                read += progress;
                                let progress = (read as f32) / size;
                                s.progress.store(progress.to_bits(), SeqCst);
                            });
                            let reader = BufReader::new(reader);
                            for line in reader.lines() {
                                let name = line.map_err(|e| e.to_string())?;
                                data.push(Username {
                                    id: name.to_ascii_lowercase(),
                                    name,
                                });
                            }
                            Ok(())
                        };

                        match load_file(path) {
                            Ok(()) => {
                                s.loaded.fetch_add(1, SeqCst);
                            }
                            Err(error) => {
                                *s.result.lock().unwrap() = Some(Err(error));
                                s.done.store(true, SeqCst);
                                return;
                            }
                        }
                    }

                    *s.result.lock().unwrap() = Some(Ok(data));
                    s.done.store(true, SeqCst);
                });
            }
        }
    }

    fn do_search(s: LoadedState, mode: SearchMode) {
        if s.pattern.len() < 3 {
            return;
        }

        let searcher = match mode {
            SearchMode::Plain => Searcher::Plain(s.pattern),
            SearchMode::NumberReplacements => {
                let pattern = s
                    .pattern
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
            SearchMode::Levenshtein => {
                let mut iter = s.edit_costs.split(',').flat_map(|x| x.parse::<u8>().ok());
                let (k, edit_cost) = match (iter.next(), iter.next(), iter.next(), iter.next()) {
                    (Some(k), Some(edit), Some(gap), transpose) => {
                        (k, levenshtein::EditCosts::new(edit, gap, 0, transpose))
                    }
                    _ => (3, levenshtein::RDAMERAU_COSTS),
                };
                Searcher::Levenshtein(s.pattern, edit_cost, k as u32)
            }
            SearchMode::RegEx => match Regex::new(&s.pattern) {
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
            let mut curr: Vec<Match> = Vec::new();
            for (i, user) in s.users.iter().enumerate() {
                if let Some(k) = searcher.matches(&user.id) {
                    curr.push(Match {
                        id: user.id.clone(),
                        name: user.name.clone(),
                        enabled: true,
                        created_at: None,
                        seen_at: None,
                        games: 0,
                        k,
                    });
                }
                if i % 1_000_000 == 0 && mode == SearchMode::Levenshtein {
                    curr.sort_unstable_by_key(|m| m.k);
                }
                if i % 1_000 == 0 {
                    s.results.lock().unwrap().append(&mut curr);
                    s.progress
                        .store(((i as f32) / (s.users.len() as f32)).to_bits(), SeqCst);
                }
            }
            s.results.lock().unwrap().append(&mut curr);
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
            saved_names: Default::default(),
            search_mode: Default::default(),
            selected: Default::default(),
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
            // Dumping(done, old) => {
            //     ui.vertical_centered_justified(|ui| {
            //         ui.label("Dumping. This may take a while.");
            //     });
            //     if done.load(SeqCst) {
            //         self.state = Loaded(std::mem::take(old));
            //     }
            // }
            Loading(s) => {
                ctx.request_repaint();
                let s = s.clone();
                ui.vertical_centered_justified(|ui| {
                    ui.label(format!("Loading {}/{}", s.loaded.load(SeqCst), s.to_load));
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
                                self.state = PickFile;
                            }
                            None => (),
                        }
                    }
                });
            }
            Loaded(s) => {
                // let mut dump = false;

                ui.horizontal(|ui| {
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
                        ui.text_edit_singleline(&mut s.edit_costs);
                    }

                    // ui.with_layout(Layout::right_to_left(), |ui| {
                    //     let hint = "Dump currently loaded usernames into a single file \
                    //                         and remove createdAt/onlineAt information for improved \
                    //                         loading performance and decreased disk usage";
                    //     dump = ui.button("Dump compact").on_hover_text(hint).clicked();
                    // });
                });

                // if dump {
                //     let users = s.users.clone();
                //     let old = std::mem::take(s);
                //     let done = Arc::new(AtomicBool::new(false));
                //     self.state = Dumping(done.clone(), old);
                //     dump_compact(users, done);
                //     return;
                // }

                ui.separator();

                let results = s.results.clone();
                let mut results = results.lock().unwrap();
                let mut do_search = false;
                let mut do_update = false;

                ui.add_enabled_ui(!s.processing.load(SeqCst), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Username: ");
                        do_search = ui.text_edit_singleline(&mut s.pattern).lost_focus();
                        do_search |= ui.button("Search").clicked();
                        ui.label(format!(
                            "Matches {}/{:}",
                            results.len(),
                            s.users.len().to_formatted_string(&Locale::en)
                        ));
                        if self.selected.len() == results.len() {
                            if ui.button("Deselect all").clicked() {
                                self.selected.clear();
                            }
                        } else if ui.button("Select all").clicked() {
                            self.selected.extend(0..results.len());
                        }
                        if ui.button("Copy").clicked() {
                            let mut names = Vec::with_capacity(self.selected.len());
                            for i in &self.selected {
                                names.push(format!("/{}", results[*i].name));
                            }
                            copy_to_clipboard(names.join("\n"));
                        }
                        ui.add_space(20.0);
                        if self.saved_names.is_empty() {
                            ui.label("Empty list");
                        } else {
                            ui.label(format!("List: {} names", self.saved_names.len()));
                        }
                        if ui.button("Add to list").clicked() {
                            for i in &self.selected {
                                let u = &results[*i];
                                self.saved_names.push(u.name.clone());
                            }
                        }
                        if ui.button("Copy list").clicked() {
                            copy_to_clipboard(format!("/{}", self.saved_names.join("\n/")));
                        }
                        if ui.button("Clear list").clicked() {
                            self.saved_names.clear();
                        }
                        ui.add_space(20.0);
                        let hint = "Fetch additional information about found users from Lichess";
                        do_update = ui
                            .add_enabled(!s.processing.load(SeqCst), Button::new("Fetch from API"))
                            .on_hover_text(hint)
                            .clicked();
                    })
                });

                if do_search {
                    s.page = 0;
                    self.selected.clear();
                    results.clear();
                    App::do_search(s.clone(), self.search_mode);
                }

                if do_update {
                    self.selected.clear();
                    s.processing.store(true, SeqCst);
                    let max = results.len().min(5 * 300);
                    let names = results[..max]
                        .iter()
                        .map(|u| &u.name)
                        .cloned()
                        .collect::<Vec<String>>();
                    let progress = s.progress.clone();
                    let processing = s.processing.clone();
                    let results = s.results.clone();
                    std::thread::spawn(move || {
                        progress.store(0.2_f32.to_bits(), SeqCst);
                        let mut api_users = HashMap::new();
                        for group in names.chunks(300) {
                            match ureq::post("https://lichess.org/api/users")
                                .send_string(&group.join(","))
                            {
                                Ok(response) => {
                                    match response.into_json::<Vec<ApiUser>>() {
                                        Ok(users) => users.into_iter().for_each(|u| {
                                            api_users.insert(u.id.clone(), u);
                                        }),
                                        Err(error) => {
                                            show_error(&error.to_string());
                                            continue;
                                        }
                                    };
                                }
                                Err(error) => show_error(&error.to_string()),
                            }
                        }
                        let mut results = results.lock().unwrap();
                        for user in results[..max].iter_mut() {
                            if let Some(u) = api_users.remove(&user.id) {
                                user.created_at = u.created_at;
                                user.seen_at = u.seen_at;
                                user.games = u.perfs.sum_games();
                                user.enabled = !u.disabled;
                            }
                        }
                        processing.store(false, SeqCst);
                    });
                }

                ui.separator();

                if s.processing.load(SeqCst) {
                    ctx.request_repaint();
                    ui.add(
                        ProgressBar::new(f32::from_bits(s.progress.load(SeqCst))).show_percentage(),
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

                            for (i, user) in results[(s.page * self.page_size)
                                ..((s.page + 1) * self.page_size).min(results.len())]
                                .iter()
                                .enumerate()
                            {
                                let i = i + s.page * self.page_size;
                                ui.with_layout(Layout::right_to_left(), |ui| {
                                    let mut checked = self.selected.contains(&i);
                                    if ui.checkbox(&mut checked, "").changed() {
                                        if checked {
                                            self.selected.insert(i);
                                        } else {
                                            self.selected.remove(&i);
                                        }
                                    }
                                });
                                if user.enabled {
                                    ui.hyperlink_to(
                                        &user.name,
                                        &format!("https://lichess.org/@/{}", user.id),
                                    );
                                } else {
                                    ui.hyperlink_to(
                                        format!("{} 🔒", user.name),
                                        &format!("https://lichess.org/@/{}", user.id),
                                    );
                                }
                                ui.label(user.created_at.map(timeago).unwrap_or_default());
                                ui.label(user.seen_at.map(timeago).unwrap_or_default());
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

// fn dump_compact(users: Arc<Vec<User>>, done: Arc<AtomicBool>) {
//     if let Some(path) = rfd::FileDialog::new()
//         .set_file_name("compact.txt")
//         .add_filter("Text", &["txt"])
//         .save_file()
//     {
//         std::thread::spawn(move || {
//             match (|| -> Result<(), String> {
//                 let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
//                 let mut writer = BufWriter::new(file);
//                 for user in users.iter() {
//                     if user.enabled {
//                         writer
//                             .write_all(user.username.as_ref().unwrap_or(&user.id).as_bytes())
//                             .map_err(|e| e.to_string())?;
//                     }
//                 }
//                 Ok(())
//             })() {
//                 Ok(()) => {
//                     MessageDialog::new()
//                         .set_title("Success")
//                         .set_buttons(MessageButtons::Ok)
//                         .set_description("File saved")
//                         .set_level(MessageLevel::Info)
//                         .show();
//                 }
//                 Err(msg) => show_error(&msg),
//             }
//             done.store(true, SeqCst);
//         });
//     }
// }

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
