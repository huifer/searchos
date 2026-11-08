//! Evaluation dataset (RFC-015 15.3).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Dataset categories (RFC-015 15.3 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetCategory {
    StaticTechKnowledge,
    TechDocumentation,
    Github,
    NewsLatest,
    ProductApiPricing,
    ComplexComparison,
    MixedZhEn,
}

impl DatasetCategory {
    /// RFC-015 15.3 target counts for the 500-question first-stage set.
    pub fn target_count(&self) -> usize {
        match self {
            Self::StaticTechKnowledge => 100,
            Self::TechDocumentation => 100,
            Self::Github => 75,
            Self::NewsLatest => 75,
            Self::ProductApiPricing => 50,
            Self::ComplexComparison => 50,
            Self::MixedZhEn => 50,
        }
    }
}

/// One evaluation question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuestion {
    pub id: String,
    pub question: String,
    pub category: DatasetCategory,
    /// Expected keywords the evidence set should cover (lightweight
    /// relevance proxy; deeper human labeling comes later per 15.6).
    pub expected_keywords: Vec<String>,
    /// Whether a correct answer requires fresh (strict-freshness) data.
    #[serde(default)]
    pub time_sensitive: bool,
    /// Optional profile override.
    #[serde(default)]
    pub profile: Option<String>,
}

/// A loaded dataset.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvaluationDataset {
    pub dataset_id: String,
    pub created_at: String,
    pub questions: Vec<EvalQuestion>,
}

impl EvaluationDataset {
    /// Loads from a JSONL file (one question per line).
    pub fn load_jsonl(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut questions = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let q: EvalQuestion =
                serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
            questions.push(q);
        }
        Ok(Self {
            dataset_id: path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "dataset".into()),
            created_at: chrono::Utc::now().to_rfc3339(),
            questions,
        })
    }

    /// Counts per category.
    pub fn counts_by_category(&self) -> std::collections::HashMap<DatasetCategory, usize> {
        let mut m = std::collections::HashMap::new();
        for q in &self.questions {
            *m.entry(q.category).or_insert(0) += 1;
        }
        m
    }

    /// Whether per-category counts meet the RFC-015 15.3 targets.
    pub fn meets_rfc_targets(&self) -> bool {
        let counts = self.counts_by_category();
        for (cat, target) in [
            (DatasetCategory::StaticTechKnowledge, 100usize),
            (DatasetCategory::TechDocumentation, 100),
            (DatasetCategory::Github, 75),
            (DatasetCategory::NewsLatest, 75),
            (DatasetCategory::ProductApiPricing, 50),
            (DatasetCategory::ComplexComparison, 50),
            (DatasetCategory::MixedZhEn, 50),
        ] {
            if counts.get(&cat).copied().unwrap_or(0) < target {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonl() {
        let dir = std::env::temp_dir().join(format!("searchos-eval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ds.jsonl");
        std::fs::write(
            &p,
            r#"{"id":"q1","question":"What is tokio?","category":"static_tech_knowledge","expected_keywords":["runtime"]}
{"id":"q2","question":"最新 rust 版本?","category":"mixed_zh_en","expected_keywords":["rust"],"time_sensitive":true}
"#,
        )
        .unwrap();
        let ds = EvaluationDataset::load_jsonl(&p).unwrap();
        assert_eq!(ds.questions.len(), 2);
        assert_eq!(ds.questions[1].category, DatasetCategory::MixedZhEn);
        assert!(ds.questions[1].time_sensitive);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn category_targets_sum_to_500() {
        let total: usize = [
            DatasetCategory::StaticTechKnowledge,
            DatasetCategory::TechDocumentation,
            DatasetCategory::Github,
            DatasetCategory::NewsLatest,
            DatasetCategory::ProductApiPricing,
            DatasetCategory::ComplexComparison,
            DatasetCategory::MixedZhEn,
        ]
        .iter()
        .map(|c| c.target_count())
        .sum();
        assert_eq!(total, 500);
    }
}
