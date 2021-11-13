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

pub fn fetch_users(names: &[String]) -> anyhow::Result<Vec<ApiUser>> {
    let result = ureq::post("https://lichess.org/api/users")
        .send_string(&names.join(","))?
        .into_json::<Vec<ApiUser>>()?;
    Ok(result)
}

pub fn close_account(name: &str, api_key: &str) -> anyhow::Result<()> {
    let resp = ureq::post(&format!("https://lichess.org/mod/{}/close", name))
        .set("Authorization", &format!("Bearer {}", api_key))
        .call()?;
    ensure!(
        resp.status() < 400,
        "Error {} when closing {}: {}",
        resp.status(),
        name,
        resp.status_text()
    );
    Ok(())
}
