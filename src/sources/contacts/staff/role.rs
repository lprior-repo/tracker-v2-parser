use crate::model::{CoachRole, Sport};

pub fn parse_role(label: &str) -> Option<CoachRole> {
    let lowered = label.to_ascii_lowercase();
    let normalized = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || NON_COACHING_EXACT.contains(&normalized.as_str()) {
        return None;
    }
    if NON_COACHING.iter().any(|token| normalized.contains(token)) {
        return None;
    }
    let assistant = ASSISTANT_TITLES
        .iter()
        .any(|title| normalized.contains(title));
    let directed = DIRECTOR_TITLES
        .iter()
        .any(|title| normalized.contains(title));
    let coached = normalized.contains("coach");
    if coached && HEAD_TITLES.iter().any(|title| normalized.contains(title)) {
        return Some(CoachRole::HeadCoach);
    }
    if directed
        && !assistant
        && !normalized.contains("assistant")
        && !normalized.contains("asst")
        && !normalized.contains("associate")
    {
        return Some(CoachRole::AthleticDirector);
    }
    if coached && assistant {
        return Some(CoachRole::AssistantCoach);
    }
    if coached && EVENT_TITLES.iter().any(|title| normalized.contains(title)) {
        return Some(CoachRole::AssistantCoach);
    }
    if EVENT_TITLES.contains(&normalized.as_str())
        || DEDICATED_TITLES.contains(&normalized.as_str())
    {
        return Some(CoachRole::AssistantCoach);
    }
    if coached {
        return Some(CoachRole::Unknown);
    }
    None
}

#[allow(dead_code)]
pub fn parse_sport(label: &str) -> Option<Sport> {
    let lowered = label.to_ascii_lowercase();
    if lowered.trim().is_empty() {
        return None;
    }
    if lowered.contains("cross country")
        || lowered.contains("cross-country")
        || lowered.contains("xc")
    {
        return Some(Sport::CrossCountry);
    }
    if lowered.contains("indoor") && lowered.contains("track") {
        return Some(Sport::Indoor);
    }
    if lowered.contains("track") {
        return Some(Sport::Outdoor);
    }
    None
}

const NON_COACHING: [&str; 17] = [
    "student",
    "player",
    "manager",
    "trainer",
    "manager",
    "assistant manager",
    "volunteer",
    "parent",
    "donor",
    "board",
    "committee",
    "secretary",
    "treasurer",
    "president",
    "assistant ad",
    "district ad",
    "office manager",
];

const NON_COACHING_EXACT: [&str; 5] = ["other", "none", "n/a", "na", "unknown"];

const DIRECTOR_TITLES: [&str; 3] = ["director", "head of", "director of"];

const HEAD_TITLES: [&str; 4] = [
    "head coach",
    "co-head coach",
    "head varsity coach",
    "head track",
];

const ASSISTANT_TITLES: [&str; 6] = [
    "assistant",
    "asst",
    "associate",
    "assistant varsity",
    "varsity assistant",
    "co-assistant",
];

const DEDICATED_TITLES: [&str; 17] = [
    "throws",
    "throw",
    "distance",
    "middle distance",
    "sprints",
    "sprint",
    "hurdles",
    "relays",
    "jumps",
    "jump",
    "pole vault",
    "high jump",
    "long jump",
    "triple jump",
    "shot put",
    "discus",
    "javelin",
];

const EVENT_TITLES: [&str; 17] = [
    "throws",
    "throw",
    "distance",
    "middle distance",
    "sprints",
    "sprint",
    "hurdles",
    "relays",
    "jumps",
    "jump",
    "pole vault",
    "high jump",
    "long jump",
    "triple jump",
    "shot put",
    "discus",
    "javelin",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_coach_is_identified() {
        assert_eq!(parse_role("Head Coach"), Some(CoachRole::HeadCoach));
        assert_eq!(parse_role("head coach"), Some(CoachRole::HeadCoach));
        assert_eq!(parse_role("Co-Head Coach"), Some(CoachRole::HeadCoach));
        assert_eq!(parse_role("Head Varsity Coach"), Some(CoachRole::HeadCoach));
        assert_eq!(
            parse_role("Girls Cross Country Head Coach"),
            Some(CoachRole::HeadCoach)
        );
    }

    #[test]
    fn assistant_coach_is_identified() {
        assert_eq!(
            parse_role("Assistant Coach"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(parse_role("asst coach"), Some(CoachRole::AssistantCoach));
        assert_eq!(
            parse_role("Assistant Varsity Coach"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(
            parse_role("Varsity Assistant Coach"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(
            parse_role("Assistant Coach Varsity"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(
            parse_role("Coach Assistant"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(
            parse_role("Girls Cross Country Assistant Coach"),
            Some(CoachRole::AssistantCoach)
        );
    }

    #[test]
    fn event_specific_roles_are_assistant_coaches() {
        assert_eq!(parse_role("Throws"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Throw"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Distance"), Some(CoachRole::AssistantCoach));
        assert_eq!(
            parse_role("Middle Distance"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(parse_role("Sprints"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Sprint"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Hurdles"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Relays"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Jumps"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Jump"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Pole Vault"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("High Jump"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Long Jump"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Triple Jump"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Shot Put"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Discus"), Some(CoachRole::AssistantCoach));
        assert_eq!(parse_role("Javelin"), Some(CoachRole::AssistantCoach));
        assert_eq!(
            parse_role("Assistant Throws Coach"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(
            parse_role("Assistant Jumps Coach"),
            Some(CoachRole::AssistantCoach)
        );
        assert_eq!(
            parse_role("Assistant Distance Coach"),
            Some(CoachRole::AssistantCoach)
        );
    }

    #[test]
    fn athletic_director_is_identified() {
        assert_eq!(
            parse_role("Athletic Director"),
            Some(CoachRole::AthleticDirector)
        );
        assert_eq!(
            parse_role("Director of Athletics"),
            Some(CoachRole::AthleticDirector)
        );
        assert_eq!(parse_role("Director"), Some(CoachRole::AthleticDirector));
        assert_eq!(
            parse_role("Director of Sports"),
            Some(CoachRole::AthleticDirector)
        );
        assert_eq!(
            parse_role("Director, Athletics"),
            Some(CoachRole::AthleticDirector)
        );
        assert_eq!(
            parse_role("Director, Sports"),
            Some(CoachRole::AthleticDirector)
        );
    }

    #[test]
    fn non_coaching_roles_are_excluded() {
        assert_eq!(parse_role("Student"), None);
        assert_eq!(parse_role("Player"), None);
        assert_eq!(parse_role("Manager"), None);
        assert_eq!(parse_role("Trainer"), None);
        assert_eq!(parse_role("Assistant Manager"), None);
        assert_eq!(parse_role("Volunteer"), None);
        assert_eq!(parse_role("Parent"), None);
        assert_eq!(parse_role("Donor"), None);
        assert_eq!(parse_role("Board Member"), None);
        assert_eq!(parse_role("Committee Chair"), None);
        assert_eq!(parse_role("Secretary"), None);
        assert_eq!(parse_role("Treasurer"), None);
        assert_eq!(parse_role("President"), None);
        assert_eq!(parse_role("Vice President"), None);
        assert_eq!(parse_role("Other"), None);
        assert_eq!(parse_role("None"), None);
        assert_eq!(parse_role("N/A"), None);
        assert_eq!(parse_role("NA"), None);
        assert_eq!(parse_role("Unknown"), None);
    }

    #[test]
    fn generic_coach_is_unknown() {
        assert_eq!(parse_role("Coach"), Some(CoachRole::Unknown));
        assert_eq!(parse_role("Track Coach"), Some(CoachRole::Unknown));
        assert_eq!(parse_role("Cross Country Coach"), Some(CoachRole::Unknown));
    }

    #[test]
    fn parse_sport_identifies_track_sports() {
        assert_eq!(parse_sport("Track"), Some(Sport::Outdoor));
        assert_eq!(parse_sport("track"), Some(Sport::Outdoor));
        assert_eq!(parse_sport("Indoor Track"), Some(Sport::Indoor));
        assert_eq!(parse_sport("Outdoor Track"), Some(Sport::Outdoor));
        assert_eq!(parse_sport("Cross Country"), Some(Sport::CrossCountry));
        assert_eq!(parse_sport("cross-country"), Some(Sport::CrossCountry));
        assert_eq!(parse_sport("XC"), Some(Sport::CrossCountry));
        assert_eq!(parse_sport("Basketball"), None);
        assert_eq!(parse_sport(""), None);
    }
}
