//! Research depth is independent of the length of the final write-up.
use std::collections::HashSet;

use super::{ToolCall, ToolOutcome};

const MIN_SEARCHES: usize = 6;
const MIN_PAGES: usize = 4;
const MIN_ROUNDS: usize = 3;
const MAX_CALLS: usize = 64;
const MAX_ROUNDS: usize = 24;
const MAX_STALLED_ROUNDS: usize = 4;

#[derive(Default)]
pub(super) struct ResearchProgress {
    queries: HashSet<String>,
    pages: HashSet<String>,
    rounds: usize,
    tool_rounds: usize,
    calls: usize,
    stalled_rounds: usize,
}

impl ResearchProgress {
    pub(super) fn searches(&self) -> usize {
        self.queries.len()
    }

    pub(super) fn ready(&self) -> bool {
        self.searches() >= MIN_SEARCHES
            && self.pages.len() >= MIN_PAGES
            && self.rounds >= MIN_ROUNDS
    }

    pub(super) fn exhausted(&self) -> bool {
        self.calls >= MAX_CALLS
            || self.tool_rounds >= MAX_ROUNDS
            || self.stalled_rounds >= MAX_STALLED_ROUNDS
    }

    pub(super) fn next_tool(&self) -> &'static str {
        if self.searches() >= 3 && self.pages.len() < MIN_PAGES {
            "fetch_url"
        } else {
            "web_search"
        }
    }

    pub(super) fn record_round(&mut self, results: &[(ToolCall, ToolOutcome)]) {
        self.tool_rounds += 1;
        self.calls += results.len();
        let before = self.queries.len() + self.pages.len();
        for (call, outcome) in results {
            if !outcome.ok || outcome.text.trim().is_empty() {
                continue;
            }
            if call.name == "web_search" && !outcome.text.starts_with("No results found for ") {
                if let Some(query) = call.arguments.get("query").and_then(|value| value.as_str()) {
                    let query = query
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_lowercase();
                    if !query.is_empty() {
                        self.queries.insert(query);
                    }
                }
            } else if call.name == "fetch_url"
                && !outcome.text.contains("but extracted no readable text.")
                && let Some(url) = call.arguments.get("url").and_then(|value| value.as_str())
                && let Ok(mut url) = reqwest::Url::parse(url)
            {
                url.set_fragment(None);
                self.pages.insert(url.to_string());
            }
        }
        if self.queries.len() + self.pages.len() > before {
            self.rounds += 1;
            self.stalled_rounds = 0;
        } else {
            self.stalled_rounds += 1;
        }
    }

    pub(super) fn note(&self) -> String {
        let progress = format!(
            "Research evidence so far: {} distinct successful searches, {} pages read, {} research rounds.",
            self.searches(),
            self.pages.len(),
            self.rounds
        );
        if self.exhausted() {
            format!(
                "{progress} Research budget or progress limit reached. Stop tools and synthesize the available evidence now. This does NOT establish that evidence is sufficient. Disclose access failures, unverified claims, and unresolved gaps; never invent sources."
            )
        } else if !self.ready() {
            format!(
                "{progress} Continue investigating before the final answer: at least {MIN_SEARCHES} distinct successful searches and {MIN_PAGES} pages read across {MIN_ROUNDS} rounds. Next call {}. These are minimum coverage checks, not a reason to pad with duplicates. Follow new leads, read primary sources, and test the strongest counterargument. Concise output requires the same research depth.",
                self.next_tool()
            )
        } else {
            format!(
                "{progress} Minimum coverage reached; audit the evidence before deciding to finish. Pursue unresolved questions, conflicting accounts, and important claims still supported only by snippets. For broad questions aim for 12–20 focused searches and 8–12 substantive pages, stopping when additional research no longer changes the findings. Keep a compact evidence ledger with source URLs, dates, claims and caveats for synthesis."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result(name: &str, key: &str, value: &str, ok: bool) -> (ToolCall, ToolOutcome) {
        let mut args = serde_json::Map::new();
        args.insert(key.into(), json!(value));
        (
            ToolCall {
                id: value.into(),
                name: name.into(),
                arguments: args.into(),
            },
            if ok {
                ToolOutcome::text("Evidence")
            } else {
                ToolOutcome::soft_failure("Unavailable")
            },
        )
    }

    #[test]
    fn one_search_or_snippets_alone_cannot_finish_research() {
        let mut progress = ResearchProgress::default();
        for i in 0..10 {
            progress.record_round(&[result("web_search", "query", &format!("angle {i}"), true)]);
        }
        assert!(!progress.ready());
        assert_eq!(progress.next_tool(), "fetch_url");
    }

    #[test]
    fn coverage_requires_searches_pages_and_follow_up_rounds() {
        let mut progress = ResearchProgress::default();
        let searches: Vec<_> = (0..6)
            .map(|i| result("web_search", "query", &format!("angle {i}"), true))
            .collect();
        progress.record_round(&searches);
        let pages: Vec<_> = (0..4)
            .map(|i| {
                result(
                    "fetch_url",
                    "url",
                    &format!("https://example.com/{i}"),
                    true,
                )
            })
            .collect();
        progress.record_round(&pages);
        assert!(!progress.ready());
        progress.record_round(&[result("web_search", "query", "counterevidence", true)]);
        assert!(progress.ready());
        assert!(!progress.exhausted());
    }

    #[test]
    fn duplicates_and_failures_do_not_inflate_coverage_or_loop_forever() {
        let mut progress = ResearchProgress::default();
        progress.record_round(&[result("web_search", "query", " First  Query ", true)]);
        for _ in 0..MAX_STALLED_ROUNDS {
            progress.record_round(&[
                result("web_search", "query", "first query", true),
                result("fetch_url", "url", "https://example.com", false),
            ]);
        }
        assert_eq!(progress.searches(), 1);
        assert!(!progress.ready());
        assert!(progress.exhausted());
        assert!(progress.note().contains("does NOT establish"));
    }

    #[test]
    fn empty_search_results_and_url_fragments_do_not_count_as_new_evidence() {
        let mut progress = ResearchProgress::default();
        let (call, _) = result("web_search", "query", "missing", true);
        progress.record_round(&[(call, ToolOutcome::text("No results found for missing."))]);
        progress.record_round(&[
            result("fetch_url", "url", "https://example.com/page#a", true),
            result("fetch_url", "url", "https://example.com/page#b", true),
        ]);
        assert_eq!(progress.searches(), 0);
        assert_eq!(progress.pages.len(), 1);
    }

    #[test]
    fn limits_bound_even_productive_research() {
        let mut progress = ResearchProgress::default();
        for round in 0..MAX_ROUNDS {
            progress.record_round(&[result(
                "web_search",
                "query",
                &format!("new angle {round}"),
                true,
            )]);
        }
        assert!(progress.exhausted());
        let mut progress = ResearchProgress::default();
        let batch: Vec<_> = (0..MAX_CALLS)
            .map(|i| result("web_search", "query", &format!("query {i}"), true))
            .collect();
        progress.record_round(&batch);
        assert!(progress.exhausted());
    }
}
