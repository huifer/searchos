//! Acceptance tests for the Context Assembly invariants (RFC-067 + RFC-068).

use super::*;

fn budget(max_tokens: usize, reserved_tokens: usize) -> ContextBudget {
    ContextBudget {
        max_tokens,
        reserved_tokens,
        ..ContextBudget::default()
    }
}

fn chunk(id: &str, source: &str, reference: &str, content: &str, score: f32) -> EvidenceChunk {
    EvidenceChunk::new(id, source, reference, content, score)
}

#[test]
fn default_budget_is_bounded_and_valid() {
    let b = ContextBudget::default();
    assert!(b.validate().is_ok());
    assert_eq!(b.usable_tokens(), 7_000);
    assert!(b.preserve_citations && b.include_conflicts && b.deduplicate);
}

#[test]
fn invalid_budgets_are_rejected_not_silently_assembled() {
    let cases = vec![
        ContextBudget {
            max_tokens: 0,
            ..Default::default()
        },
        ContextBudget {
            max_tokens: 10,
            reserved_tokens: 10,
            ..Default::default()
        },
        ContextBudget {
            max_sources: 0,
            ..Default::default()
        },
        ContextBudget {
            max_chunks_per_source: 0,
            ..Default::default()
        },
    ];
    for b in cases {
        assert!(b.validate().is_err());
        assert!(assemble(&[], &b, &WordCountEstimator).is_err());
    }
}

#[test]
fn empty_input_produces_a_complete_empty_block() {
    let block = assemble(&[], &budget(20, 2), &WordCountEstimator).unwrap();
    assert!(block.evidence.is_empty());
    assert!(block.citation_refs.is_empty());
    assert!(block.dropped.is_empty());
    assert!(!block.truncated);
    assert_eq!(block.status, ContextStatus::Complete);
    assert_eq!(block.token_usage.used_tokens, 0);
    assert_eq!(block.token_usage.usable_tokens, 18);
}

#[test]
fn permission_denied_evidence_never_enters_the_context_and_is_audited() {
    let mut denied = chunk("denied", "s1", "https://secret/x", "secret text", 99.0);
    denied.permission_ok = false;
    let allowed = chunk("allowed", "s1", "https://ok/x", "one two", 1.0);

    let block = assemble(
        &[denied.clone(), allowed.clone()],
        &budget(20, 2),
        &WordCountEstimator,
    )
    .unwrap();

    assert_eq!(block.evidence.len(), 1);
    assert_eq!(block.evidence[0].chunk_id, "allowed");
    assert!(!block.citation_refs.iter().any(|r| r.contains("secret")));
    assert!(block
        .dropped
        .iter()
        .any(|d| d.reason == DropReason::PermissionDenied && d.chunk_id == "denied"));
}

#[test]
fn dedup_keeps_the_best_primary_and_preserves_every_occurrence() {
    let mut a = chunk("c1", "srcA", "https://a/x", "one two", 1.0);
    a.content_hash = Some("h1".into());
    let mut b = chunk("c2", "srcB", "https://b/y", "one two", 5.0);
    b.content_hash = Some("h1".into());
    let mut c = chunk("c3", "srcC", "https://c/z", "one two", 3.0);
    c.content_hash = Some("h1".into());

    let block = assemble(&[a, b, c], &budget(20, 2), &WordCountEstimator).unwrap();

    assert_eq!(block.evidence.len(), 1);
    let e = &block.evidence[0];
    assert_eq!(e.chunk_id, "c2", "highest-scoring occurrence is primary");
    assert_eq!(e.occurrences, 3);
    assert_eq!(e.seen_in_sources, vec!["srcA", "srcB", "srcC"]);
    // Reposts are provenance, NOT three independent sources.
    assert_eq!(block.citation_refs.len(), 1);
    assert_eq!(
        block
            .dropped
            .iter()
            .filter(|d| d.reason == DropReason::Duplicate)
            .count(),
        2
    );
}

#[test]
fn dedup_can_be_disabled() {
    let mut a = chunk("a", "srcA", "https://a/x", "one", 1.0);
    a.content_hash = Some("same".into());
    let mut b = chunk("b", "srcB", "https://b/y", "two", 2.0);
    b.content_hash = Some("same".into());
    let bgt = ContextBudget {
        deduplicate: false,
        ..budget(30, 2)
    };
    let block = assemble(&[a, b], &bgt, &WordCountEstimator).unwrap();
    assert_eq!(block.evidence.len(), 2);
    assert!(block.dropped.is_empty());
}

#[test]
fn conflicting_evidence_is_inherited_through_dedup() {
    let mut calm = chunk("calm", "srcA", "https://a/x", "one", 9.0);
    calm.content_hash = Some("h".into());
    let mut rebuttal = chunk("rebuttal", "srcB", "https://b/y", "two", 1.0);
    rebuttal.content_hash = Some("h".into());
    rebuttal.conflicting = true;

    let block = assemble(&[calm, rebuttal], &budget(20, 2), &WordCountEstimator).unwrap();
    assert!(block.evidence[0].conflicting);
    assert_eq!(block.evidence[0].occurrences, 2);
}

#[test]
fn source_and_per_source_caps_are_enforced() {
    let bgt = ContextBudget {
        max_sources: 2,
        max_chunks_per_source: 1,
        ..budget(50, 2)
    };
    let items = vec![
        chunk("a1", "s1", "https://s1/a1", "one", 10.0),
        chunk("a2", "s1", "https://s1/a2", "two", 9.0),
        chunk("b1", "s2", "https://s2/b1", "three", 8.0),
        chunk("c1", "s3", "https://s3/c1", "four", 7.0),
        chunk("b2", "s2", "https://s2/b2", "five", 6.0),
    ];
    let block = assemble(&items, &bgt, &WordCountEstimator).unwrap();
    assert_eq!(
        block
            .evidence
            .iter()
            .map(|e| e.chunk_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a1", "b1"]
    );
    assert_eq!(
        block
            .dropped
            .iter()
            .filter(|d| d.reason == DropReason::ChunksPerSourceCap)
            .count(),
        2
    );
    assert_eq!(
        block
            .dropped
            .iter()
            .filter(|d| d.reason == DropReason::SourceCap)
            .count(),
        1
    );
}

#[test]
fn counter_evidence_ranks_first_and_is_never_crowded_out() {
    // usable = 8. Three citations cost 2 each -> content pool = 2.
    // The low-scoring rebuttal must receive content before the high scorer.
    let high = chunk("high", "s1", "https://s1/high", "one two three four", 10.0);
    let mid = chunk("mid", "s2", "https://s2/mid", "alpha beta", 5.0);
    let mut rebuttal = chunk("rebuttal", "s3", "https://s3/rebuttal", "bad", 1.0);
    rebuttal.conflicting = true;

    let block = assemble(&[high, mid, rebuttal], &budget(10, 2), &WordCountEstimator).unwrap();

    assert_eq!(block.evidence.len(), 3);
    assert!(block.evidence[0].conflicting);
    assert_eq!(block.evidence[0].content, "bad");
    assert!(!block.evidence[0].truncated);
    assert_eq!(block.evidence[1].chunk_id, "high");
    assert_eq!(block.evidence[1].content.trim_end(), "one");
    assert!(block.evidence[1].truncated);
    assert_eq!(block.status, ContextStatus::BudgetLimited);
    assert!(block.truncated);
    assert_eq!(block.citation_refs.len(), 3);
}

#[test]
fn conflicts_can_be_explicitly_excluded() {
    let normal = chunk("normal", "s1", "https://s1/normal", "one", 1.0);
    let mut rebuttal = chunk("rebuttal", "s2", "https://s2/rebuttal", "two", 9.0);
    rebuttal.conflicting = true;
    let bgt = ContextBudget {
        include_conflicts: false,
        ..budget(20, 2)
    };
    let block = assemble(&[normal, rebuttal], &bgt, &WordCountEstimator).unwrap();
    assert_eq!(block.evidence.len(), 1);
    assert_eq!(block.evidence[0].chunk_id, "normal");
    assert_eq!(block.citation_refs, vec!["https://s1/normal"]);
    assert!(block
        .dropped
        .iter()
        .any(|d| d.reason == DropReason::ConflictExcluded));
}

#[test]
fn content_is_always_a_verbatim_prefix_never_a_generated_summary() {
    let items = vec![
        chunk("a", "s1", "https://s1/a", "one two three four five", 5.0),
        chunk("b", "s2", "https://s2/b", "alpha beta gamma delta", 4.0),
        chunk("c", "s3", "https://s3/c", "中文检索系统详细说明", 3.0),
    ];
    let originals: HashMap<String, String> = items
        .iter()
        .map(|c| (c.chunk_id.clone(), c.content.clone()))
        .collect();
    let block = assemble(&items, &budget(9, 1), &HeuristicEstimator).unwrap();

    assert!(!block.evidence.is_empty());
    for e in &block.evidence {
        let original = &originals[&e.chunk_id];
        assert!(
            original.starts_with(&e.content),
            "content must be a verbatim prefix: {:?} vs {:?}",
            original,
            e.content
        );
        assert!(!e.content.contains('…'));
        assert!(!e.canonical_ref.is_empty());
    }
    assert_eq!(
        block.citation_refs,
        block
            .evidence
            .iter()
            .map(|e| e.canonical_ref.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn cjk_assembly_is_char_boundary_safe_and_within_budget() {
    let items = vec![chunk(
        "cjk",
        "s1",
        "https://s1/cjk",
        "中文检索系统会把证据组装成适合 Agent 上下文的结构",
        1.0,
    )];
    let bgt = ContextBudget {
        max_tokens: 12,
        reserved_tokens: 2,
        ..ContextBudget::default()
    };
    let block = assemble(&items, &bgt, &HeuristicEstimator).unwrap();
    let e = &block.evidence[0];
    assert!(items[0].content.starts_with(&e.content));
    assert!(e.content.is_char_boundary(e.content.len()));
    assert!(block.token_usage.used_tokens <= block.token_usage.usable_tokens);
}

#[test]
fn citations_survive_even_when_all_content_is_truncated_away() {
    let items = vec![
        chunk("a", "s1", "https://s1/a", "one two three", 3.0),
        chunk("b", "s2", "https://s2/b", "four five six", 2.0),
    ];
    // usable = 4 -> exactly two 2-token citations, zero content pool.
    let block = assemble(&items, &budget(6, 2), &WordCountEstimator).unwrap();
    assert_eq!(block.evidence.len(), 2);
    assert!(block.evidence.iter().all(|e| e.content.is_empty()));
    assert!(block.evidence.iter().all(|e| e.truncated));
    assert_eq!(block.citation_refs, vec!["https://s1/a", "https://s2/b"]);
    assert_eq!(block.token_usage.used_tokens, 4);
    assert_eq!(block.status, ContextStatus::BudgetLimited);
}

#[test]
fn citation_limited_status_when_even_references_do_not_fit() {
    let items = vec![
        chunk("a", "s1", "https://s1/a", "one", 2.0),
        chunk("b", "s2", "https://s2/b", "two", 1.0),
    ];
    // usable = 2 -> only one 2-token citation fits.
    let block = assemble(&items, &budget(3, 1), &WordCountEstimator).unwrap();
    assert_eq!(block.evidence.len(), 1);
    assert_eq!(block.citation_refs.len(), 1);
    assert_eq!(block.status, ContextStatus::CitationLimited);
    assert!(block.truncated);
    assert!(block
        .dropped
        .iter()
        .any(|d| d.reason == DropReason::CitationBudget));
    assert!(block.token_usage.used_tokens <= block.token_usage.usable_tokens);
}

#[test]
fn disabling_citation_preservation_drops_instead_of_emitting_empty_stubs() {
    let items = vec![
        chunk("a", "s1", "https://s1/a", "one two", 2.0),
        chunk("b", "s2", "https://s2/b", "three", 1.0),
    ];
    let bgt = ContextBudget {
        preserve_citations: false,
        ..budget(3, 1)
    };
    let block = assemble(&items, &bgt, &WordCountEstimator).unwrap();
    assert_eq!(block.evidence.len(), 1);
    assert_eq!(block.evidence[0].content, "one two");
    assert!(block
        .dropped
        .iter()
        .any(|d| d.reason == DropReason::TokenBudget));
    assert_eq!(block.status, ContextStatus::BudgetLimited);
}

#[test]
fn reserved_tokens_are_never_available_to_evidence() {
    let items = vec![chunk("a", "s1", "https://s1/a", "one two three four", 1.0)];
    let tight = assemble(&items, &budget(10, 8), &WordCountEstimator).unwrap();
    let loose = assemble(&items, &budget(10, 1), &WordCountEstimator).unwrap();
    assert_eq!(tight.evidence[0].content, "");
    assert!(tight.truncated);
    assert_eq!(loose.evidence[0].content, "one two three four");
    assert!(!loose.truncated);
    assert_eq!(loose.status, ContextStatus::Complete);
}

#[test]
fn used_tokens_always_matches_the_sum_of_evidence_costs() {
    let items: Vec<EvidenceChunk> = (0..12)
        .map(|i| {
            chunk(
                &format!("c{i:02}"),
                &format!("s{}", i % 4),
                &format!("https://s{}/c{i}", i % 4),
                "one two three four five six",
                (12 - i) as f32,
            )
        })
        .collect();
    for (max, reserved) in [(6, 1), (17, 2), (40, 5), (200, 10)] {
        let block = assemble(&items, &budget(max, reserved), &WordCountEstimator).unwrap();
        let sum: usize = block.evidence.iter().map(|e| e.estimated_tokens).sum();
        assert_eq!(sum, block.token_usage.used_tokens);
        assert!(sum <= block.token_usage.usable_tokens);
        assert!(block.evidence.iter().all(|e| !e.canonical_ref.is_empty()));
    }
}

#[test]
fn assembly_is_deterministic_with_stable_tie_breaking() {
    let items = vec![
        chunk("zeta", "s1", "https://s1/zeta", "one", 1.0),
        chunk("alpha", "s2", "https://s2/alpha", "two", 1.0),
        chunk("mid", "s3", "https://s3/mid", "three", 1.0),
    ];
    let bgt = budget(50, 2);
    let first = assemble(&items, &bgt, &WordCountEstimator).unwrap();
    let second = assemble(&items, &bgt, &WordCountEstimator).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first
            .evidence
            .iter()
            .map(|e| e.chunk_id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "mid", "zeta"]
    );
    // serde round-trip keeps the block stable for transport.
    let json = serde_json::to_string(&first).unwrap();
    let back: ContextBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(first, back);
}

#[test]
fn convenience_wrapper_uses_the_heuristic_estimator() {
    let items = vec![chunk("a", "s1", "https://s1/a", "中文检索", 1.0)];
    let bgt = budget(20, 2);
    let via_wrapper = assemble_with_default_estimator(&items, &bgt).unwrap();
    let direct = assemble(&items, &bgt, &HeuristicEstimator).unwrap();
    assert_eq!(via_wrapper, direct);
}
