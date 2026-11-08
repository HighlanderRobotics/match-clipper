//! Upload titles and descriptions use official event and match results.
use crate::clip::{Plan, PlannedClip};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Event {
    name: String,
    year: u32,
    #[serde(default)]
    remap_teams: HashMap<String, String>,
}
#[derive(Deserialize)]
struct Alliance {
    team_keys: Vec<String>,
    score: i64,
}
#[derive(Deserialize)]
struct Alliances {
    red: Alliance,
    blue: Alliance,
}
#[derive(Deserialize)]
struct Match {
    key: String,
    comp_level: String,
    match_number: u32,
    set_number: u32,
    alliances: Alliances,
}
#[derive(serde::Serialize)]
pub struct UploadText {
    pub title: String,
    pub description: String,
}
fn match_name(m: &Match) -> Result<String> {
    Ok(match m.comp_level.as_str() {
        "qm" => format!("Qualification {}", m.match_number),
        "ef" => format!("Octofinal {} Match {}", m.set_number, m.match_number),
        "qf" => format!("Quarterfinal {} Match {}", m.set_number, m.match_number),
        "sf" => format!("Semifinal {} Match {}", m.set_number, m.match_number),
        "f" => format!("Final {}", m.match_number),
        _ => bail!("Unknown competition level for {}", m.key),
    })
}
fn render(event: &Event, m: &Match, plan: &Plan, clip: &PlannedClip) -> Result<UploadText> {
    if m.key != clip.match_key || m.alliances.red.score < 0 || m.alliances.blue.score < 0 {
        bail!("Final result unavailable for {}", clip.match_key);
    }
    let teams = |a: &Alliance| -> Result<String> {
        if a.team_keys.is_empty() {
            bail!("Alliance teams missing for {}", m.key);
        }
        a.team_keys
            .iter()
            .map(|key| {
                let alias = event.remap_teams.get(key).unwrap_or(key);
                let label = alias
                    .strip_prefix("frc")
                    .filter(|s| !s.is_empty())
                    .with_context(|| format!("Invalid team key for {}", m.key))?;
                let original: Vec<_> = event
                    .remap_teams
                    .iter()
                    .filter(|(_, v)| *v == alias)
                    .map(|(k, _)| k)
                    .collect();
                if original.len() > 1 {
                    bail!("Ambiguous FMS mapping for {label}");
                }
                if let Some(fms) = original.first() {
                    let number = fms
                        .strip_prefix("frc")
                        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                        .context("Invalid FMS team number")?;
                    Ok(format!("{label} ({number})"))
                } else if label.bytes().all(|b| b.is_ascii_digit()) {
                    Ok(label.to_owned())
                } else {
                    bail!("Missing FMS mapping for {label}")
                }
            })
            .collect::<Result<Vec<_>>>()
            .map(|v| v.join(", "))
    };
    let event_name = if event.name.contains(&event.year.to_string()) {
        event.name.clone()
    } else {
        format!("{} {}", event.name, event.year)
    };
    let name = match_name(m)?;
    Ok(UploadText {
        title: format!("{name} | {event_name}"),
        description: format!(
            "{event_name}\n{name}\n\nRed alliance: {}\nBlue alliance: {}\nFinal score: Red {} - Blue {}\n\nTBA match: https://www.thebluealliance.com/match/{}\nTeams and final results from The Blue Alliance.\n\nSource stream: https://www.youtube.com/watch?v={}&t={}s\nStream interval: {}-{} seconds\n\nClipped by FRC 8033 (Highlander Robotics).\nSource code: {}\nMatch footage from the linked event livestream.",
            teams(&m.alliances.red)?, teams(&m.alliances.blue)?,
            m.alliances.red.score, m.alliances.blue.score, m.key,
            plan.video_id, clip.start_seconds, clip.start_seconds, clip.end_seconds,
            env!("CARGO_PKG_REPOSITORY"),
        ),
    })
}
pub async fn load(http: &Client, plan: &Plan) -> Result<HashMap<String, UploadText>> {
    let key = std::env::var("TBA_API_KEY")
        .or_else(|_| std::env::var("TBA_KEY"))
        .context("Set TBA_API_KEY or TBA_KEY")?;
    let event: Event = http
        .get(format!(
            "https://www.thebluealliance.com/api/v3/event/{}",
            plan.event
        ))
        .header("X-TBA-Auth-Key", &key)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let matches: Vec<Match> = http
        .get(format!(
            "https://www.thebluealliance.com/api/v3/event/{}/matches",
            plan.event
        ))
        .header("X-TBA-Auth-Key", &key)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    plan.clips
        .iter()
        .map(|clip| {
            let m = matches
                .iter()
                .find(|m| m.key == clip.match_key)
                .with_context(|| format!("Match results missing for {}", clip.match_key))?;
            Ok((clip.match_key.clone(), render(&event, m, plan, clip)?))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Event, Match, Plan) {
        let event = Event {
            name: "MadTown Throwdown".into(),
            year: 2026,
            remap_teams: HashMap::from([
                ("frc9987".into(), "frc8033B".into()),
                ("frc9979".into(), "frc1678C".into()),
            ]),
        };
        let m = Match {
            key: "2026mttd_qm4".into(),
            comp_level: "qm".into(),
            match_number: 4,
            set_number: 1,
            alliances: Alliances {
                red: Alliance {
                    team_keys: vec!["frc1538".into(), "frc114".into(), "frc971".into()],
                    score: 301,
                },
                blue: Alliance {
                    team_keys: vec!["frc8033B".into(), "frc1678C".into(), "frc973".into()],
                    score: 132,
                },
            },
        };
        let plan = Plan {
            event: "2026mttd".into(),
            video_id: "CI6Dm6TSHzs".into(),
            privacy: "public".into(),
            skipped_without_actual_time: 0,
            clips: vec![PlannedClip {
                match_key: m.key.clone(),
                start_seconds: 381,
                end_seconds: 567,
            }],
        };
        (event, m, plan)
    }
    #[test]
    fn descriptions_preserve_aliases_final_scores_and_sources() {
        let (e, m, p) = fixture();
        let text = render(&e, &m, &p, &p.clips[0]).unwrap();
        assert_eq!(text.title, "Qualification 4 | MadTown Throwdown 2026");
        for expected in [
            "Red alliance: 1538, 114, 971",
            "Blue alliance: 8033B (9987), 1678C (9979), 973",
            "Final score: Red 301 - Blue 132",
            "https://www.thebluealliance.com/match/2026mttd_qm4",
            "CI6Dm6TSHzs&t=381s",
            "Clipped by FRC 8033",
            "https://github.com/HighlanderRobotics/match-clipper",
        ] {
            assert!(text.description.contains(expected), "missing {expected}");
        }
    }
    #[test]
    fn incomplete_results_are_not_published_as_final_scores() {
        let (e, mut m, p) = fixture();
        m.alliances.red.score = -1;
        assert!(render(&e, &m, &p, &p.clips[0]).is_err());
        m.alliances.red.score = 301;
        m.alliances.blue.team_keys.clear();
        assert!(render(&e, &m, &p, &p.clips[0]).is_err());
    }
    #[test]
    fn numeric_fms_keys_and_aliases_render_identically() {
        let (e, mut m, p) = fixture();
        let alias = render(&e, &m, &p, &p.clips[0]).unwrap().description;
        m.alliances.blue.team_keys[0] = "frc9987".into();
        assert_eq!(render(&e, &m, &p, &p.clips[0]).unwrap().description, alias);
        m.alliances.blue.team_keys[0] = "frc1234B".into();
        assert!(render(&e, &m, &p, &p.clips[0]).is_err());
    }
    #[test]
    fn playoff_titles_include_set_and_match() {
        let (_, mut m, _) = fixture();
        m.comp_level = "sf".into();
        m.set_number = 2;
        m.match_number = 1;
        assert_eq!(match_name(&m).unwrap(), "Semifinal 2 Match 1");
        m.comp_level = "f".into();
        assert_eq!(match_name(&m).unwrap(), "Final 1");
    }
}
