use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use triple_accel::levenshtein::{self, EditCosts};

pub struct Username {
    pub id: String,
    pub name: String,
}

pub struct Match {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: Option<DateTime<Utc>>,
    pub seen_at: Option<DateTime<Utc>>,
    pub games: u32,
    pub k: u32,
}

impl  Match {
    pub fn new(user: &Username, k: u32) -> Self {
        Self {
            id: user.id.clone(),
            name: user.name.clone(),
            enabled: true,
            created_at: None,
            seen_at: None,
            games: 0,
            k,
        }
    }
}

impl From<&str> for Match {
    fn from(name: &str) -> Self {
        Self {
            id: name.to_ascii_lowercase(),
            name: name.to_string(),
            enabled: true,
            created_at: None,
            seen_at: None,
            games: 0,
            k: 0,
        }
    }
}

impl From<&String> for Match {
    fn from(name: &String) -> Self {
        name.into()
    }
}

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct LoadingState {
    pub progress: Arc<AtomicU32>,
    pub done: Arc<AtomicBool>,
    pub result: Arc<Mutex<Option<Result<Vec<Username>, String>>>>,
}

impl LoadingState {
    pub fn new() -> Self {
        Self {
            progress: Arc::new(AtomicU32::new(0.0_f32 as u32)),
            done: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Clone, Default)]
pub struct LoadedState {
    pub pattern: String,
    pub users: Arc<Vec<Username>>,
    pub results: Arc<Mutex<Vec<Match>>>,
    pub processing: Arc<AtomicBool>,
    pub page: usize,
    pub progress: Arc<AtomicUsize>,
    pub progress_max: Arc<AtomicUsize>,
}

pub enum State {
    PickFile,
    AskPassword(PathBuf),
    Loading(LoadingState),
    Loaded(LoadedState),
}

impl State {
    pub fn loaded(users: Vec<Username>) -> Self {
        Self::Loaded(LoadedState {
            pattern: String::new(),
            users: Arc::new(users),
            results: Default::default(),
            processing: Default::default(),
            page: 0,
            progress: Arc::new(AtomicUsize::new(0)),
            progress_max: Arc::new(AtomicUsize::new(1)),
        })
    }
}

impl Default for State {
    fn default() -> Self {
        Self::PickFile
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Plain,
    NumberReplacements,
    Levenshtein,
    RegEx,
}

#[derive(Clone)]
pub enum Searcher {
    Plain(String),
    Regex(Regex),
    Levenshtein(String, LevenshteinSettings),
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Plain
    }
}

#[derive(Deserialize, Serialize, Clone, Copy)]
pub struct LevenshteinSettings {
    pub max_k: u32,
    pub mismatch_cost: u8,
    pub gap_cost: u8,
    pub swap_cost: u8,
}

impl LevenshteinSettings {
    pub fn edit_costs(&self) -> EditCosts {
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
