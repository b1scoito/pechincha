//! Signal-based relevance scoring for search results.
//!
//! Replaces hard-coded accessory word lists and magic thresholds with three
//! independent signals that combine into a single 0.0–1.0 relevance score:
//!
//! 1. **Title Structure** — Where the query appears in the title. Products have
//!    the query at the start; accessories bury it after "for/para/compatible".
//! 2. **Price Clustering** — Where the price sits relative to the result set.
//!    Accessories cluster at the bottom; products cluster at the top.
//! 3. **String Similarity** — How closely the title matches the query text.
//!    Products have focused titles; accessories dilute with compatibility lists.

use rust_decimal::Decimal;

/// Minimum score to keep a product in results.
pub const RELEVANT_THRESHOLD: f64 = 0.40;

/// Minimum score for a Keepa ASIN candidate.
pub const KEEPA_CANDIDATE_THRESHOLD: f64 = 0.50;

/// Combined relevance score from all signals.
#[derive(Debug, Clone)]
pub struct RelevanceScore {
    pub total: f64,
    pub title_structure: f64,
    pub price_cluster: f64,
    pub string_similarity: f64,
}

/// Score a product by combining all signals.
///
/// `gap_ratio` is the max price gap ratio from `price_cluster_scores()`.
/// When there's a clear bimodal split (ratio > 3x), the cluster weight is
/// boosted so that cheap accessories with misleading titles still get killed.
pub fn score_product(title: &str, query: &str, cluster_score: f64, gap_ratio: f64) -> RelevanceScore {
    let s1 = title_structure_score(title, query);
    let s2 = cluster_score;
    let s3 = string_similarity_score(title, query);

    // Dynamic weights: when there's a clear bimodal price gap (>3x),
    // boost the cluster weight so cheap accessories with perfect titles
    // still get killed by price clustering.
    let (w_structure, w_similarity, w_cluster) = if gap_ratio > 3.0 {
        (0.35, 0.25, 0.40)
    } else {
        (0.45, 0.35, 0.20)
    };

    // When the cluster score is low (item priced far below MSRP), dampen
    // the contribution of title-based signals proportionally.  This prevents
    // accessories with good titles (brand at start, query tokens present)
    // from passing on title score alone.
    //
    // At cluster 0.0 → dampen = 0.0: title signals contribute nothing
    // At cluster 0.24 (40% MSRP) → dampen = 0.6: title contribution halved
    // At cluster ≥ 0.40 (60%+ MSRP) → dampen = 1.0: full title contribution
    let dampen = (s2 / 0.40).min(1.0);
    let title_contribution = (s1 * w_structure + s3 * w_similarity) * dampen;
    let total = title_contribution + s2 * w_cluster;

    RelevanceScore {
        total,
        title_structure: s1,
        price_cluster: s2,
        string_similarity: s3,
    }
}

// ── Signal 1: Title Structure ───────────────────────────────────────────────

/// Score based on WHERE the query appears in the title.
///
/// Products: "[QUERY] additional description" → high score
/// Accessories: "replacement part for [QUERY]" → low score
fn title_structure_score(title: &str, query: &str) -> f64 {
    let title_norm = normalize(title);
    let query_norm = normalize(query);
    let title_len = title_norm.len() as f64;

    if title_len == 0.0 {
        return 0.0;
    }

    let tokens: Vec<&str> = query_norm.split_whitespace().collect();
    if tokens.is_empty() {
        return 0.5;
    }

    // 1. Find where each query token appears in the title
    let mut positions: Vec<usize> = Vec::new();
    let mut found_count = 0;

    for token in &tokens {
        if let Some(pos) = title_norm.find(token) {
            positions.push(pos);
            found_count += 1;
        } else {
            // Try compact form (no spaces) for compound tokens like "v15"
            let title_compact = title_norm.replace(' ', "");
            if title_compact.contains(token) {
                found_count += 1;
                positions.push(0); // approximate
            }
        }
    }

    if found_count == 0 {
        return 0.0;
    }

    // Token coverage: what fraction of query tokens were found
    let coverage = found_count as f64 / tokens.len() as f64;
    if coverage < 1.0 {
        return coverage * 0.2;
    }

    let first_pos = positions.iter().copied().min().unwrap_or(0);
    let last_pos = positions.iter().copied().max().unwrap_or(0);

    // 2. Position score: earlier = better (0.0–1.0)
    let position_ratio = first_pos as f64 / title_len;
    let position_score = if position_ratio < 0.1 {
        1.0
    } else if position_ratio < 0.25 {
        0.85
    } else if position_ratio < 0.5 {
        0.5
    } else {
        0.2
    };

    // 3. Exact phrase bonus: query appears as contiguous phrase
    let phrase_bonus = if title_norm.contains(&query_norm) {
        0.2
    } else {
        0.0
    };

    // 4. Spread penalty: in a real product listing, query tokens cluster
    //    together near the start (e.g. "Dreame L50 Ultra Robot Vacuum").
    //    In accessories, they're scattered across a long title — brand early,
    //    model buried in a compatibility list ("Escova Dreame ... para ... L50 Ultra").
    //    This replaces hard-coded compatibility marker word lists.
    let spread = (last_pos - first_pos) as f64;
    let query_byte_len = query_norm.len() as f64;
    // How much wider are the tokens spread vs. the query itself?
    // A spread ≤ query length means they're adjacent (product). A spread
    // of 3× the query length means heavy scattering (accessory).
    let spread_ratio = if query_byte_len > 0.0 {
        spread / query_byte_len
    } else {
        0.0
    };
    let spread_penalty = if spread_ratio <= 1.5 {
        0.0   // tokens are adjacent or nearly so — product title
    } else if spread_ratio <= 3.0 {
        0.25  // moderate scatter
    } else {
        0.5   // heavy scatter — tokens are far apart, likely a compatibility list
    };

    let raw: f64 = position_score + phrase_bonus - spread_penalty;
    raw.clamp(0.0, 1.0)
}

// ── Signal 2: Price Clustering ──────────────────────────────────────────────

/// Score each product based on its price relative to the expected product price.
///
/// When `msrp_brl` is available (from Keepa), it's the authoritative reference:
/// products within 20%-250% of MSRP are likely the actual product; anything
/// below 15% is almost certainly an accessory. This is the strongest signal
/// we have — it's the manufacturer's own price.
///
/// When no MSRP is available, falls back to gap detection in the price
/// distribution to separate accessories from products.
///
/// Returns `(scores, gap_ratio)` where `scores` is a Vec in the same order
/// as the input, and `gap_ratio` is the detected price separation ratio.
pub fn price_cluster_scores(prices: &[Decimal], msrp_brl: Option<f64>) -> (Vec<f64>, f64) {
    if prices.is_empty() {
        return (vec![], 1.0);
    }

    let valid_prices: Vec<f64> = prices
        .iter()
        .filter(|p| **p > Decimal::ZERO)
        .filter_map(|p| p.to_string().parse::<f64>().ok())
        .collect();

    if valid_prices.is_empty() {
        return (prices.iter().map(|_| 0.5).collect(), 1.0);
    }

    // ── MSRP-anchored scoring (when available) ──────────────────────────
    // This is the strongest price signal: the manufacturer's reference price.
    if let Some(msrp) = msrp_brl {
        if msrp > 0.0 {
            let gap_ratio = {
                let mut sorted = valid_prices.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                if sorted.len() >= 4 {
                    let mut max_r = 1.0f64;
                    for i in 1..sorted.len() {
                        if sorted[i - 1] > 0.0 {
                            max_r = max_r.max(sorted[i] / sorted[i - 1]);
                        }
                    }
                    max_r
                } else {
                    1.0
                }
            };

            let scores = prices
                .iter()
                .map(|p| {
                    let price = p.to_string().parse::<f64>().unwrap_or(0.0);
                    if price <= 0.0 {
                        return 0.3;
                    }
                    let ratio = price / msrp;
                    // Smooth curve: score rises continuously from 0 to 1 as ratio
                    // approaches 1.0 (MSRP), then falls gently above MSRP.
                    // This avoids cliff edges where a small price change flips
                    // the score from "accessory" to "product".
                    if ratio < 0.10 {
                        0.0
                    } else if ratio < 0.65 {
                        // Ramp from 0.0 at 10% to 0.40 at 65% of MSRP.
                        // Items at 50% of MSRP get ~0.29 — with dampening,
                        // not enough for accessories to pass.
                        let t = (ratio - 0.10) / 0.55; // 0..1 over 10%..65%
                        t * 0.40
                    } else if ratio <= 3.0 {
                        // 65%-300%: score peaks at 1.0 near MSRP, falls gently
                        let distance = (ratio - 1.0).abs();
                        (1.0 - distance * 0.3).clamp(0.50, 1.0)
                    } else {
                        0.4 // > 300% — overpriced but might be a bundle
                    }
                })
                .collect();

            return (scores, gap_ratio);
        }
    }

    // ── Gap-detection fallback (no MSRP available) ──────────────────────
    // Find the split between accessories and products by looking for significant
    // price gaps (>2x between consecutive sorted prices).  When multiple gaps
    // exist (e.g. cheap accessories, mid-range accessories, expensive products),
    // prefer the HIGHEST split point — the gap closest to the expensive cluster
    // is most likely the accessory/product boundary.
    let mut sorted = valid_prices.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let (product_min, product_max, detected_gap_ratio) = if sorted.len() >= 4 {
        // Find the biggest consecutive price gap — this is the most likely
        // boundary between accessories and the actual product.
        let mut max_gap_ratio = 0.0f64;
        let mut gap_index = 0;
        for i in 1..sorted.len() {
            if sorted[i - 1] > 0.0 {
                let ratio = sorted[i] / sorted[i - 1];
                if ratio > max_gap_ratio {
                    max_gap_ratio = ratio;
                    gap_index = i;
                }
            }
        }

        if max_gap_ratio > 2.0 {
            let upper = &sorted[gap_index..];
            // Accept the split if the upper cluster has at least 3 items
            // (avoids splitting on a single outlier) OR the gap is very large.
            if upper.len() >= 3 || max_gap_ratio > 3.0 {
                (upper[0], *upper.last().unwrap(), max_gap_ratio)
            } else {
                (*sorted.first().unwrap(), *sorted.last().unwrap(), max_gap_ratio)
            }
        } else {
            (*sorted.first().unwrap(), *sorted.last().unwrap(), max_gap_ratio)
        }
    } else {
        (*sorted.first().unwrap(), *sorted.last().unwrap(), 1.0)
    };

    let median = sorted[sorted.len() / 2];

    let scores = prices
        .iter()
        .map(|p| {
            let price = p.to_string().parse::<f64>().unwrap_or(0.0);
            if price <= 0.0 {
                return 0.3;
            }

            if price >= product_min && price <= product_max {
                let distance = (price - median).abs() / median;
                (1.0 - distance * 0.3).clamp(0.5, 1.0)
            } else if price < product_min {
                let ratio = price / product_min;
                (ratio * 0.4).clamp(0.0, 0.35)
            } else {
                let ratio = product_max / price;
                (ratio * 0.7).clamp(0.3, 0.8)
            }
        })
        .collect();

    (scores, detected_gap_ratio)
}

// ── Signal 3: String Similarity ─────────────────────────────────────────────

/// Score how similar the title is to the query using token-level comparison.
fn string_similarity_score(title: &str, query: &str) -> f64 {
    let title_norm = normalize(title);
    let query_norm = normalize(query);

    if query_norm.is_empty() || title_norm.is_empty() {
        return 0.0;
    }

    // Full string similarity (captures exact matches well)
    let full_sim = strsim::normalized_damerau_levenshtein(&query_norm, &title_norm);

    // Token-level similarity: for each query token, find best matching title token
    let query_tokens: Vec<&str> = query_norm.split_whitespace().collect();
    let title_tokens: Vec<&str> = title_norm.split_whitespace().collect();

    if query_tokens.is_empty() || title_tokens.is_empty() {
        return full_sim;
    }

    let mut token_scores = Vec::new();
    for qt in &query_tokens {
        let best = title_tokens
            .iter()
            .map(|tt| strsim::jaro_winkler(qt, tt))
            .fold(0.0f64, |a, b| a.max(b));
        token_scores.push(best);
    }

    let token_avg = token_scores.iter().sum::<f64>() / token_scores.len() as f64;

    // Title length penalty: very long titles (accessories with compatibility lists)
    // score lower than focused product titles.
    let length_ratio = query_norm.len() as f64 / title_norm.len() as f64;
    let length_factor = if length_ratio > 0.3 {
        1.0 // Title is focused
    } else if length_ratio > 0.1 {
        0.85 // Moderate title length
    } else {
        0.7 // Very long title — probably a compatibility list
    };

    (token_avg * length_factor).max(full_sim)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Cheap pre-filter: all query tokens must appear in the title.
/// Used before prices are available (for CDP fetch decisions).
pub fn tokens_match(title: &str, query: &str) -> bool {
    let stop_words: &[&str] = &[
        "de", "do", "da", "dos", "das", "para", "com", "sem", "por", "em", "no", "na",
        "the", "for", "with", "and", "or", "a", "an", "o", "e", "um", "uma",
    ];

    let title_norm = normalize(title);
    let title_compact = title_norm.replace(' ', "");

    let query_tokens: Vec<String> = normalize(query)
        .split_whitespace()
        .filter(|t| t.len() > 1 && !stop_words.contains(t))
        .map(|s| s.to_string())
        .collect();

    if query_tokens.is_empty() {
        return true;
    }

    query_tokens
        .iter()
        .all(|token| title_norm.contains(token.as_str()) || title_compact.contains(token.as_str()))
}

/// Normalize text for comparison: lowercase, strip diacritics, split hyphens.
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .replace('-', " ")
        .replace('_', " ")
        // Strip common Portuguese diacritics for comparison
        .replace('á', "a")
        .replace('à', "a")
        .replace('ã', "a")
        .replace('â', "a")
        .replace('é', "e")
        .replace('ê', "e")
        .replace('í', "i")
        .replace('ó', "o")
        .replace('ô', "o")
        .replace('õ', "o")
        .replace('ú', "u")
        .replace('ü', "u")
        .replace('ç', "c")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_title_scores_high() {
        let score = title_structure_score("Sennheiser HD 600 - Audiophile Open-Back", "Sennheiser HD 600");
        assert!(score > 0.8, "Product title should score high: {score}");
    }

    #[test]
    fn accessory_title_scores_low() {
        let score = title_structure_score(
            "Almofadas de Gel Refrescante Para Sennheiser HD 600",
            "Sennheiser HD 600",
        );
        assert!(score < 0.5, "Accessory title should score low: {score}");
    }

    #[test]
    fn cable_accessory_scores_low() {
        let score = title_structure_score(
            "Cabo balanceado XLR 4 pinos para HD-6XX, HD 600, HD 650",
            "Sennheiser HD 600",
        );
        assert!(score < 0.4, "Cable accessory should score low: {score}");
    }

    #[test]
    fn exact_product_scores_highest() {
        let s1 = score_product("Dyson V15 Detect Cordless Vacuum", "Dyson V15 Detect", 0.5, 1.0);
        let s2 = score_product("Filtro HEPA para Dyson V15 Detect V11 V10", "Dyson V15 Detect", 0.5, 1.0);
        assert!(
            s1.total > s2.total,
            "Product ({:.2}) should outscore accessory ({:.2})",
            s1.total,
            s2.total
        );
    }

    #[test]
    fn similarity_prefers_focused_title() {
        let s1 = string_similarity_score("Sennheiser HD 600", "Sennheiser HD 600");
        let s2 = string_similarity_score(
            "Replacement ear pads cushion compatible with Sennheiser HD 600 HD 650 HD 660S",
            "Sennheiser HD 600",
        );
        assert!(s1 > s2, "Focused title ({s1:.2}) > long accessory title ({s2:.2})");
    }

    #[test]
    fn tokens_match_basic() {
        assert!(tokens_match("Sennheiser HD 600 Open Back", "Sennheiser HD 600"));
        assert!(!tokens_match("Sennheiser HD 650 Open Back", "Sennheiser HD 600"));
        assert!(tokens_match("Dyson V15 Detect Plus", "Dyson V15 Detect"));
    }

    #[test]
    fn price_clustering_separates_accessories() {
        let prices: Vec<Decimal> = [50, 80, 120, 2000, 2200, 2500, 2600]
            .iter()
            .map(|&p| Decimal::from(p))
            .collect();
        // Without MSRP — gap detection
        let (scores, gap_ratio) = price_cluster_scores(&prices, None);
        assert!(scores[0] < scores[4], "R$50 ({:.2}) < R$2200 ({:.2})", scores[0], scores[4]);
        assert!(scores[2] < scores[3], "R$120 ({:.2}) < R$2000 ({:.2})", scores[2], scores[3]);
        assert!(gap_ratio > 3.0, "Gap ratio should be > 3.0: {gap_ratio:.2}");
    }

    #[test]
    fn brand_before_marker_still_penalized() {
        // Accessory where brand "Dreame" appears before "para" but model "L50 Ultra" after
        let score = title_structure_score(
            "Escova Original Dreame DuoBrush para Aspiradores Robot L50 Ultra",
            "Dreame L50 Ultra",
        );
        assert!(score < 0.5, "Brand-before-marker accessory should score low: {score}");
    }

    #[test]
    fn price_veto_kills_cheap_accessory_with_good_title() {
        // Accessory at 0.5% of MSRP with great title — cluster=0.0 should veto
        let s = score_product(
            "Escova Original Dreame DuoBrush para Aspiradores Robot L50 Ultra",
            "Dreame L50 Ultra",
            0.0,  // cluster: <15% of MSRP
            5.0,  // clear bimodal gap
        );
        assert!(
            s.total < RELEVANT_THRESHOLD,
            "Cheap accessory (cluster=0) must be below threshold: {:.2}",
            s.total
        );
    }

    #[test]
    fn price_veto_passes_real_product() {
        // Real product at ~60% of MSRP with good title
        let s = score_product(
            "DREAME L50 Ultra Robot Vacuum and Mop White",
            "Dreame L50 Ultra",
            0.88,  // cluster: ~60% of MSRP
            5.0,
        );
        assert!(
            s.total >= RELEVANT_THRESHOLD,
            "Real product should pass threshold: {:.2}",
            s.total
        );
    }

    #[test]
    fn branded_cable_at_40pct_msrp_filtered() {
        // Cable at ~40% of MSRP with brand at title start.
        // New smooth ramp gives cluster ~0.24 at 40%, not 0.82.
        let cluster_at_40pct = 0.24; // (0.40 - 0.10) / 0.50 * 0.40
        let s = score_product(
            "SENNHEISER 4 pinos XLR cabo balanceado para HD-6XX, HD 600, HD 650",
            "Sennheiser HD 600",
            cluster_at_40pct,
            5.0,
        );
        assert!(
            s.total < RELEVANT_THRESHOLD,
            "Cable at 40% MSRP should be filtered: {:.3} (struct={:.2}, cluster={:.2}, sim={:.2})",
            s.total, s.title_structure, s.price_cluster, s.string_similarity
        );
    }

    #[test]
    fn gap_detection_multi_cluster() {
        // Three clusters: cheap accessories (50-600), mid accessories (1000-1500),
        // actual products (4000-10000).  The split should happen at the highest
        // gap (1500→4000), not between the two accessory sub-clusters.
        let prices: Vec<Decimal> = [50, 100, 200, 400, 600, 1000, 1200, 1500, 4000, 8000, 9000, 10000]
            .iter()
            .map(|&p| Decimal::from(p))
            .collect();
        let (scores, gap_ratio) = price_cluster_scores(&prices, None);
        // Gap at 1500→4000 = 2.67x
        assert!(gap_ratio > 2.0, "Should detect significant gap: {gap_ratio:.2}");
        // Products (4000+) should score much higher than accessories (50-1500)
        assert!(scores[8] >= 0.5, "R$4000 should score high: {:.2}", scores[8]);
        assert!(scores[0] < 0.35, "R$50 should score low: {:.2}", scores[0]);
        assert!(scores[5] < 0.35, "R$1000 should score low: {:.2}", scores[5]);
        assert!(scores[7] < 0.35, "R$1500 should score low: {:.2}", scores[7]);
    }

    #[test]
    fn msrp_anchored_kills_accessories() {
        let prices: Vec<Decimal> = [30, 50, 100, 250, 1000, 2000, 2500, 3000]
            .iter()
            .map(|&p| Decimal::from(p))
            .collect();
        // MSRP = R$ 2500 (e.g. Sennheiser HD 600)
        let (scores, _) = price_cluster_scores(&prices, Some(2500.0));
        // R$30 is 1.2% of MSRP — should score ~0
        assert!(scores[0] < 0.05, "R$30 at MSRP R$2500 should be ~0: {:.2}", scores[0]);
        // R$100 is 4% of MSRP — should score very low
        assert!(scores[2] < 0.05, "R$100 at MSRP R$2500 should be ~0: {:.2}", scores[2]);
        // R$250 is 10% of MSRP — right at the ramp start
        assert!(scores[3] < 0.05, "R$250 at MSRP R$2500 should be ~0: {:.2}", scores[3]);
        // R$1000 is 40% of MSRP — on the ramp, should be moderate (not high)
        assert!(scores[4] < 0.30, "R$1000 at MSRP R$2500 should be moderate: {:.2}", scores[4]);
        assert!(scores[4] > 0.10, "R$1000 at MSRP R$2500 shouldn't be zero: {:.2}", scores[4]);
        // R$2500 is 100% of MSRP — should score high
        assert!(scores[6] > 0.8, "R$2500 at MSRP R$2500 should be high: {:.2}", scores[6]);
        // Smooth ramp: prices on the ramp should score progressively higher
        assert!(scores[3] < scores[4], "R$250 < R$1000 in score");
        assert!(scores[4] < scores[5], "R$1000 < R$2000 in score");
    }
}
