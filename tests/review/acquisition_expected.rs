pub const HEADERS: [&str; 18] = [
    "Checkpoint ID",
    "Source",
    "Kind",
    "State",
    "School",
    "School source ID",
    "Athlete",
    "Athlete source ID",
    "URL",
    "Status",
    "Attempts",
    "Durable state",
    "Retry after UTC",
    "Error",
    "Last success",
    "Created",
    "Next action",
    "AI guidance",
];

pub fn rows(ids: &[i64]) -> Vec<Vec<String>> {
    const GUIDANCE: &str = "Use public evidence and exact source IDs. Do not bypass login, paywalls, challenges or saved stops; do not infer missing results.";
    const WAIT: &str = "Wait for the automatic retry. Use public evidence and exact source IDs. Do not bypass login, paywalls, challenges or saved stops; do not infer missing results.";
    const INVESTIGATE: &str =
        "Investigate checkpoint and retained evidence before an authorized retry.";
    const BLOCKED: &str = "Human handling required; preserve the access stop.";
    const CREATED: &str = "2026-10-29T12:00:00.000000Z";
    let fields = [
        [
            "milesplit",
            "athlete",
            "WI",
            "Published School",
            "1000",
            "Alex Morgan",
            "11",
            "https://wi.milesplit.com/athletes/11",
            "failed",
            "3",
            "",
            "",
            "DNS resolution failed",
            "",
            CREATED,
            INVESTIGATE,
            GUIDANCE,
        ],
        [
            "milesplit",
            "athlete",
            "WI",
            "",
            "",
            "",
            "12",
            "https://wi.milesplit.com/athletes/12",
            "blocked",
            "0",
            "",
            "",
            "Challenge requires human handling",
            "",
            CREATED,
            BLOCKED,
            GUIDANCE,
        ],
        [
            "milesplit",
            "athlete",
            "WI",
            "",
            "",
            "",
            "13",
            "https://wi.milesplit.com/athletes/13",
            "failed",
            "3",
            "deferred",
            "2026-10-29T13:00:00.000000Z",
            "Connection timed out",
            "",
            CREATED,
            "Wait for automatic retry at the recorded time; preserve attempt counters and saved stops.",
            WAIT,
        ],
        [
            "athleticnet",
            "directory",
            "",
            "",
            "",
            "",
            "",
            "https://www.athletic.net/api/v1/public/GetStatesCountries2",
            "failed",
            "1",
            "",
            "",
            "Root request failed",
            "",
            CREATED,
            INVESTIGATE,
            GUIDANCE,
        ],
        [
            "athleticnet",
            "athlete",
            "WI",
            "",
            "",
            "",
            "18",
            "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=18&sport=tf&level=0",
            "blocked",
            "0",
            "",
            "",
            "Provider disabled; existing stop retained",
            "",
            CREATED,
            BLOCKED,
            GUIDANCE,
        ],
    ];
    assert_eq!(ids.len(), fields.len());
    ids.iter()
        .zip(fields)
        .map(|(id, fields)| {
            std::iter::once(id.to_string())
                .chain(fields.into_iter().map(str::to_owned))
                .collect()
        })
        .collect()
}
