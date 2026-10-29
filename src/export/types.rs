#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportLayout {
    #[default]
    Detailed,
    SingleSheet,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub states: Vec<String>,
    pub graduation_years: Vec<i32>,
    pub rows_per_file: usize,
    pub per_athlete_sheets: bool,
}

#[derive(Debug, Clone)]
pub struct ExportIssue {
    pub source: String,
    pub url: String,
    pub detail: String,
}
