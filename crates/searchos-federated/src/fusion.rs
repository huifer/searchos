//! Trust-weighted rank fusion across federated sources (RFC-062 §9.2,
//! RFC-066).
//!
//! Cross-source fusion is RANK-based: raw scores from different sources are
//! not comparable, so only per-source ranks are combined, and every list's
//! influence comes from its reliability weight — never from a provider score.
//!
//! Score(d) = Σ_s w_s / (k + rank_s(d)),  initial k = 60.
//!
//! `searchos-retrieval::rrf_fuse_weighted` is the same math over the
//! retrieval-domain candidate type (chunk-id keyed); this module is the
//! generic, kernel-side form over the federated candidate type (or any
//! payload keyed by an opaque string), so the executor and the pipeline can
//! fuse source outputs without dragging storage types into the kernel.

/// One source's ranked contribution to a cross-source fusion.
pub struct RankedSourceList<'a, T> {
    /// Stable source label (audit only; fusion itself is key-based).
    pub source_id: &'a str,
    /// Reliability weight for the whole list (RFC-066: from the source's
    /// `ReliabilityProfile`, e.g. `success_rate` — never a raw score).
    /// Non-negative; zero removes the list's influence.
    pub weight: f32,
    /// The source's ranked items, best first.
    pub items: &'a [T],
}

/// Fuses per-source ranked lists into ONE deterministic order.
///
/// Items are identified by `key_of` (for federated sources the canonical
/// reference). The returned payload for a key comes from the FIRST list that
/// contains it, so fusion never rewrites content — it only orders. Ties are
/// broken by key so the result is reproducible across runs (P-011
/// Explainable Decision / RFC-062 replay gates).
pub fn fuse_ranks<T: Clone>(
    lists: &[RankedSourceList<'_, T>],
    key_of: impl Fn(&T) -> String,
    k: f32,
) -> Vec<T> {
    let mut scores: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    // Insertion order preservation: keep the first list's payload per key and
    // a deterministic key order for the tie-break.
    let mut order: Vec<String> = Vec::new();
    let mut payload: std::collections::HashMap<String, T> = std::collections::HashMap::new();

    for source in lists {
        for (rank, item) in source.items.iter().enumerate() {
            let key = key_of(item);
            let contribution = source.weight / (k + (rank as f32 + 1.0));
            if let Some(entry) = scores.get_mut(&key) {
                *entry += contribution;
            } else {
                scores.insert(key.clone(), contribution);
                order.push(key.clone());
            }
            payload.entry(key).or_insert_with(|| item.clone());
        }
    }

    let mut out: Vec<T> = order
        .into_iter()
        .filter_map(|key| payload.remove(&key))
        .collect();
    out.sort_by(|a, b| {
        let (ka, kb) = (key_of(a), key_of(b));
        let (sa, sb) = (
            scores.get(&ka).copied().unwrap_or(0.0),
            scores.get(&kb).copied().unwrap_or(0.0),
        );
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Descending score, ascending key on ties.
            .then_with(|| ka.cmp(&kb))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(url: &str) -> String {
        url.to_string()
    }

    #[test]
    fn higher_weight_rank_wins_over_equal_rank() {
        // Same rank in two lists; the heavier source's item must rank first.
        let a = vec![item("https://a.dev/1"), item("https://a.dev/2")];
        let b = vec![item("https://b.dev/1"), item("https://b.dev/2")];
        let fused = fuse_ranks(
            &[
                RankedSourceList {
                    source_id: "heavy",
                    weight: 1.0,
                    items: &a,
                },
                RankedSourceList {
                    source_id: "light",
                    weight: 0.2,
                    items: &b,
                },
            ],
            |i| i.clone(),
            60.0,
        );
        assert_eq!(fused[0], "https://a.dev/1");
        assert_eq!(fused[2], "https://b.dev/1");
    }

    #[test]
    fn payload_comes_from_the_first_list_containing_the_key() {
        let first = vec![item("https://shared.dev/x")];
        let second = vec![item("https://shared.dev/x"), item("https://b.dev/1")];
        let fused = fuse_ranks(
            &[
                RankedSourceList {
                    source_id: "first",
                    weight: 1.0,
                    items: &first,
                },
                RankedSourceList {
                    source_id: "second",
                    weight: 1.0,
                    items: &second,
                },
            ],
            |i| i.clone(),
            60.0,
        );
        // The duplicated key is fused into ONE entry ranked above the
        // second-list-only item (two contributions vs one).
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0], "https://shared.dev/x");
    }

    #[test]
    fn order_is_deterministic_across_runs() {
        let a = vec![item("https://a.dev/1"), item("https://a.dev/2")];
        let b = vec![item("https://b.dev/1")];
        let run = || {
            fuse_ranks(
                &[
                    RankedSourceList {
                        source_id: "a",
                        weight: 0.7,
                        items: &a,
                    },
                    RankedSourceList {
                        source_id: "b",
                        weight: 0.7,
                        items: &b,
                    },
                ],
                |i| i.clone(),
                60.0,
            )
        };
        assert_eq!(run(), run());
        // a.dev/1 and b.dev/1 share (weight, rank) — the key tie-break must
        // pick the same winner every time.
        assert_eq!(run()[0], "https://a.dev/1");
    }

    #[test]
    fn zero_weight_removes_a_lists_influence() {
        let a = vec![item("https://a.dev/1")];
        let b = vec![item("https://b.dev/1")];
        let fused = fuse_ranks(
            &[
                RankedSourceList {
                    source_id: "a",
                    weight: 1.0,
                    items: &a,
                },
                RankedSourceList {
                    source_id: "b",
                    weight: 0.0,
                    items: &b,
                },
            ],
            |i| i.clone(),
            60.0,
        );
        assert_eq!(fused[0], "https://a.dev/1");
        // The zero-weight item is still PRESENT (never silently dropped).
        assert_eq!(fused.len(), 2);
    }
}
