use std::time::Duration;

use anyhow::ensure;
use chrono::{serde::ts_milliseconds_option, DateTime, Utc};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiUser {
    pub id: String,
    #[serde(default, with = "ts_milliseconds_option")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, with = "ts_milliseconds_option")]
    pub seen_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub perfs: ApiPerfs,
}

#[derive(Default, Deserialize)]
#[serde(default)]
#[allow(non_snake_case)]
pub struct ApiPerfs {
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
    pub fn sum_games(self) -> u32 {
        self.chess960.map_or(0, |p| p.games)
            + self.atomic.map_or(0, |p| p.games)
            + self.racingKings.map_or(0, |p| p.games)
            + self.ultraBullet.map_or(0, |p| p.games)
            + self.blitz.map_or(0, |p| p.games)
            + self.kingOfTheHill.map_or(0, |p| p.games)
            + self.bullet.map_or(0, |p| p.games)
            + self.correspondence.map_or(0, |p| p.games)
            + self.horde.map_or(0, |p| p.games)
            + self.classical.map_or(0, |p| p.games)
            + self.rapid.map_or(0, |p| p.games)
    }
}

#[derive(Deserialize)]
struct ApiPerf {
    #[serde(default)]
    games: u32,
}

pub fn fetch_users(names: &[String]) -> anyhow::Result<Vec<ApiUser>> {
    let result = ureq::post("https://lichess.org/api/users")
        .send_string(&names.join(","))?
        .into_json::<Vec<ApiUser>>()?;
    Ok(result)
}

pub fn close_account(name: &str, api_key: &str) -> anyhow::Result<()> {
    ensure!(!api_key.is_empty(), "No API key");

    let resp = ureq::post(&format!("https://lichess.org/mod/{}/close", name))
        .set("Authorization", &format!("Bearer {}", api_key))
        .call()?;
    ensure!(
        resp.status() == 200,
        "Error {} when closing {}: {}",
        resp.status(),
        name,
        resp.status_text()
    );

    std::thread::sleep(Duration::from_millis(100));

    let resp = ureq::post(&format!("https://lichess.org/api/user/{}/note", name))
        .set("Authorization", &format!("Bearer {}", api_key))
        .send_json(ureq::json!({
          "text": "Closed for username",
          "mod": true,
        }))?;
    ensure!(
        resp.status() == 200,
        "Error {} when adding the mod note to {}: {}",
        resp.status(),
        name,
        resp.status_text()
    );

    Ok(())
}
