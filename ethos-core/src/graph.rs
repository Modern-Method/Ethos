//! Spreading activation algorithm for associative memory retrieval
//!
//! This module implements graph-based retrieval modeled on hippocampal activation:
//! - Anchor nodes = top-K cosine matches (from Story 006)
//! - Spreading = iterative activation propagation through `memory_graph_links`
//! - Final score = weighted combination of similarity + activation + structural scores

use crate::config::RetrievalConfig;
use crate::error::EthosError;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

/// Maximum number of edges to load for spreading (bounds memory usage)
const MAX_EDGES: i64 = 500;

/// A node in the activation graph with scoring components
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationNode {
    pub id: Uuid,
    pub node_type: String,
    pub cosine_score: f32,
    pub spread_score: f32,
    pub structural_score: f32,
    pub final_score: f32,
}

/// An edge in the memory graph
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from_id: Uuid,
    pub to_id: Uuid,
    pub to_type: String,
    pub weight: f32,
}

/// Result of spreading activation
#[derive(Debug, Serialize, Deserialize)]
pub struct SpreadResult {
    pub nodes: Vec<ActivationNode>,
    pub iterations: u32,
    pub edges_loaded: usize,
}

/// Core spreading activation algorithm (testable without database)
///
/// # Arguments
/// * `anchors` - Initial nodes from cosine search with their similarity scores
/// * `edges` - Graph edges for propagation
/// * `config` - Retrieval configuration (spreading_strength, iterations, weights)
///
/// # Returns
/// * `SpreadResult` - Nodes ranked by combined score
pub fn spread_activation_core(
    anchors: &[ActivationNode],
    edges: &[GraphEdge],
    config: &RetrievalConfig,
) -> SpreadResult {
    if anchors.is_empty() {
        return SpreadResult {
            nodes: vec![],
            iterations: 0,
            edges_loaded: 0,
        };
    }

    // If no edges, return anchors with cosine scores only
    if edges.is_empty() {
        let nodes: Vec<ActivationNode> = anchors
            .iter()
            .map(|a| {
                let final_score = config.weight_similarity * a.cosine_score;
                ActivationNode {
                    id: a.id,
                    node_type: a.node_type.clone(),
                    cosine_score: a.cosine_score,
                    spread_score: 0.0,
                    structural_score: 0.0,
                    final_score,
                }
            })
            .collect();

        return SpreadResult {
            nodes,
            iterations: 0,
            edges_loaded: 0,
        };
    }

    // Initialize activation scores from anchors
    let mut activation: HashMap<Uuid, f32> = HashMap::new();
    let mut node_types: HashMap<Uuid, String> = HashMap::new();

    for anchor in anchors {
        activation.insert(anchor.id, anchor.cosine_score);
        node_types.insert(anchor.id, anchor.node_type.clone());
    }

    // Track which nodes exist in the graph
    for edge in edges {
        node_types.insert(edge.to_id, edge.to_type.clone());
    }

    // Build adjacency list for propagation
    let mut adjacency: HashMap<Uuid, Vec<&GraphEdge>> = HashMap::new();
    for edge in edges {
        adjacency.entry(edge.from_id).or_default().push(edge);
    }

    // Iterative spreading activation
    for _iteration in 0..config.iterations {
        let mut new_activation: HashMap<Uuid, f32> = HashMap::new();

        // For each active node
        for (node_id, &node_activation) in &activation {
            // Propagate to neighbors
            if let Some(neighbors) = adjacency.get(node_id) {
                for edge in neighbors {
                    let contribution = node_activation * edge.weight * config.spreading_strength;
                    let current = new_activation.entry(edge.to_id).or_insert(0.0);
                    *current += contribution;
                }
            }
        }

        // Merge new activation into main map (accumulates over iterations)
        for (id, score) in new_activation {
            let current = activation.entry(id).or_insert(0.0);
            *current += score;
        }
    }

    // Calculate structural scores (in-degree centrality)
    let mut in_degree: HashMap<Uuid, f32> = HashMap::new();
    let max_in_degree = edges.len() as f32;

    for edge in edges {
        let current = in_degree.entry(edge.to_id).or_insert(0.0);
        *current += 1.0;
    }

    // Build final result nodes
    let mut nodes: Vec<ActivationNode> = Vec::new();

    for (id, node_type) in &node_types {
        let cosine = anchors
            .iter()
            .find(|a| &a.id == id)
            .map(|a| a.cosine_score)
            .unwrap_or(0.0);

        let spread = activation.get(id).copied().unwrap_or(0.0);
        let structural = in_degree.get(id).copied().unwrap_or(0.0) / max_in_degree;

        let final_score = config.weight_similarity * cosine
            + config.weight_activation * spread
            + config.weight_structural * structural;

        nodes.push(ActivationNode {
            id: *id,
            node_type: node_type.clone(),
            cosine_score: cosine,
            spread_score: spread,
            structural_score: structural,
            final_score,
        });
    }

    // Sort by final score descending
    nodes.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    SpreadResult {
        nodes,
        iterations: config.iterations,
        edges_loaded: edges.len(),
    }
}

/// Run spreading activation over the memory graph
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `anchors` - Initial nodes from cosine search with their similarity scores
/// * `config` - Retrieval configuration (spreading_strength, iterations, weights)
///
/// # Returns
/// * `Ok(SpreadResult)` - Nodes ranked by combined score
/// * `Err(EthosError)` - On database errors
///
/// # Algorithm
/// 1. Load subgraph edges for anchor nodes
/// 2. Initialize activation from anchor cosine scores
/// 3. Iterate: propagate activation through edges with decay
/// 4. Calculate structural scores (in-degree centrality)
/// 5. Combine: final_score = w_sim * cosine + w_act * spread + w_str * structural
pub async fn spread_activation(
    pool: &PgPool,
    anchors: &[ActivationNode],
    config: &RetrievalConfig,
) -> Result<SpreadResult, EthosError> {
    if anchors.is_empty() {
        return Ok(SpreadResult {
            nodes: vec![],
            iterations: 0,
            edges_loaded: 0,
        });
    }

    // Extract anchor IDs for subgraph loading
    let anchor_ids: Vec<Uuid> = anchors.iter().map(|a| a.id).collect();

    // Load edges connecting to/from anchors
    let edges = load_subgraph_edges(pool, &anchor_ids).await?;

    // Run core algorithm
    Ok(spread_activation_core(anchors, &edges, config))
}

/// Load edges from memory_graph_links for the given node IDs
async fn load_subgraph_edges(pool: &PgPool, node_ids: &[Uuid]) -> Result<Vec<GraphEdge>, EthosError> {
    let rows = sqlx::query_as::<_, (Uuid, Uuid, String, f32)>(
        r#"
        SELECT from_id, to_id, to_type, weight
        FROM memory_graph_links
        WHERE from_id = ANY($1)
           OR to_id = ANY($1)
        ORDER BY weight DESC
        LIMIT $2
        "#
    )
    .bind(node_ids)
    .bind(MAX_EDGES)
    .fetch_all(pool)
    .await?;

    let edges: Vec<GraphEdge> = rows
        .into_iter()
        .map(|(from_id, to_id, to_type, weight)| GraphEdge {
            from_id,
            to_id,
            to_type,
            weight,
        })
        .collect();

    Ok(edges)
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> RetrievalConfig {
        RetrievalConfig {
            decay_factor: 0.15,
            spreading_strength: 0.85,
            iterations: 3,
            anchor_top_k_episodes: 10,
            anchor_top_k_facts: 10,
            weight_similarity: 0.5,
            weight_activation: 0.3,
            weight_structural: 0.2,
            confidence_gate: 0.12,
        }
    }

    fn make_anchor(id: Uuid, node_type: &str, cosine: f32) -> ActivationNode {
        ActivationNode {
            id,
            node_type: node_type.to_string(),
            cosine_score: cosine,
            spread_score: 0.0,
            structural_score: 0.0,
            final_score: 0.0,
        }
    }

    fn make_edge(from: Uuid, to: Uuid, to_type: &str, weight: f32) -> GraphEdge {
        GraphEdge {
            from_id: from,
            to_id: to,
            to_type: to_type.to_string(),
            weight,
        }
    }

    // ========================================================================
    // TEST 1: Single anchor with no edges returns anchor with cosine only
    // ========================================================================
    #[test]
    fn test_spread_single_anchor_no_edges() {
        let config = test_config();
        let anchor_id = Uuid::new_v4();
        let anchors = vec![make_anchor(anchor_id, "episode", 0.9)];
        let edges = vec![];

        let result = spread_activation_core(&anchors, &edges, &config);

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id, anchor_id);
        assert!((result.nodes[0].final_score - 0.45).abs() < 0.01); // 0.9 * 0.5
        assert_eq!(result.edges_loaded, 0);
    }

    // ========================================================================
    // TEST 2: Activation propagates through edges
    // ========================================================================
    #[test]
    fn test_spread_propagates_through_edges() {
        let config = test_config();
        let anchor_id = Uuid::new_v4();
        let neighbor_id = Uuid::new_v4();

        let anchors = vec![make_anchor(anchor_id, "episode", 1.0)];
        let edges = vec![make_edge(anchor_id, neighbor_id, "fact", 0.5)];

        let result = spread_activation_core(&anchors, &edges, &config);

        // Should have both nodes
        assert_eq!(result.nodes.len(), 2);

        // Neighbor should have received activation
        let neighbor_node = result.nodes.iter().find(|n| n.id == neighbor_id);
        assert!(neighbor_node.is_some());
        
        let neighbor = neighbor_node.unwrap();
        // spread_score = 1.0 * 0.5 * 0.85 * 3 iterations (accumulated)
        assert!(neighbor.spread_score > 0.0);
    }

    // ========================================================================
    // TEST 3: Activation decays with spreading strength
    // ========================================================================
    #[test]
    fn test_spread_decays_with_strength() {
        let mut config = test_config();
        config.spreading_strength = 0.5; // Lower spreading strength

        let anchor_id = Uuid::new_v4();
        let neighbor_id = Uuid::new_v4();

        let anchors = vec![make_anchor(anchor_id, "episode", 1.0)];
        let edges = vec![make_edge(anchor_id, neighbor_id, "fact", 1.0)];

        let result = spread_activation_core(&anchors, &edges, &config);

        let neighbor = result.nodes.iter().find(|n| n.id == neighbor_id).unwrap();
        // With strength=0.5, neighbor should get half the activation per iteration
        // After 3 iterations: 1.0 * 1.0 * 0.5 * 3 = 1.5 accumulated
        assert!((neighbor.spread_score - 1.5).abs() < 0.1);
    }

    // ========================================================================
    // TEST 4: Zero spreading strength returns anchors only (no spread)
    // ========================================================================
    #[test]
    fn test_spread_zero_strength_returns_anchors_only() {
        let mut config = test_config();
        config.spreading_strength = 0.0;

        let anchor_id = Uuid::new_v4();
        let neighbor_id = Uuid::new_v4();

        let anchors = vec![make_anchor(anchor_id, "episode", 1.0)];
        let edges = vec![make_edge(anchor_id, neighbor_id, "fact", 1.0)];

        let result = spread_activation_core(&anchors, &edges, &config);

        // Neighbor should have zero spread score
        let neighbor = result.nodes.iter().find(|n| n.id == neighbor_id);
        if let Some(n) = neighbor {
            assert!((n.spread_score - 0.0).abs() < 0.01);
        }
    }

    // ========================================================================
    // TEST 5: Cycles are handled safely (no infinite loops)
    // ========================================================================
    #[test]
    fn test_spread_handles_cycles_safely() {
        let config = test_config();
        let node_a = Uuid::new_v4();
        let node_b = Uuid::new_v4();

        let anchors = vec![make_anchor(node_a, "episode", 1.0)];
        // Create a cycle: A -> B -> A
        let edges = vec![
            make_edge(node_a, node_b, "episode", 0.5),
            make_edge(node_b, node_a, "episode", 0.5),
        ];

        // Should complete without hanging
        let result = spread_activation_core(&anchors, &edges, &config);

        assert_eq!(result.iterations, 3);
        assert!(result.nodes.iter().any(|n| n.id == node_a));
        assert!(result.nodes.iter().any(|n| n.id == node_b));
    }

    // ========================================================================
    // TEST 6: Final score weights sum correctly
    // ========================================================================
    #[test]
    fn test_final_score_weights_sum_correctly() {
        let config = test_config();

        // Verify weights sum to 1.0
        let weight_sum =
            config.weight_similarity + config.weight_activation + config.weight_structural;
        assert!((weight_sum - 1.0).abs() < 0.01);

        // Test score calculation with actual algorithm
        let anchor_id = Uuid::new_v4();
        let neighbor_id = Uuid::new_v4();

        let anchors = vec![make_anchor(anchor_id, "episode", 0.8)];
        let edges = vec![make_edge(anchor_id, neighbor_id, "fact", 0.6)];

        let result = spread_activation_core(&anchors, &edges, &config);

        // Anchor should have cosine-based final score
        let anchor = result.nodes.iter().find(|n| n.id == anchor_id).unwrap();
        assert!((anchor.cosine_score - 0.8).abs() < 0.01);
    }

    // ========================================================================
    // TEST 7: Empty anchor list returns empty result
    // ========================================================================
    #[test]
    fn test_spread_empty_anchors_returns_empty() {
        let config = test_config();
        let anchors: Vec<ActivationNode> = vec![];
        let edges: Vec<GraphEdge> = vec![];

        let result = spread_activation_core(&anchors, &edges, &config);

        assert!(result.nodes.is_empty());
        assert_eq!(result.iterations, 0);
    }

    // ========================================================================
    // TEST 8: Multiple anchors accumulate activation
    // ========================================================================
    #[test]
    fn test_spread_multiple_anchors_accumulate() {
        let config = test_config();
        let anchor1 = Uuid::new_v4();
        let anchor2 = Uuid::new_v4();
        let target = Uuid::new_v4();

        // Two anchors both connect to same target
        let anchors = vec![
            make_anchor(anchor1, "episode", 0.9),
            make_anchor(anchor2, "episode", 0.8),
        ];
        let edges = vec![
            make_edge(anchor1, target, "fact", 0.5),
            make_edge(anchor2, target, "fact", 0.5),
        ];

        let result = spread_activation_core(&anchors, &edges, &config);

        // Target should have accumulated activation from both anchors
        let target_node = result.nodes.iter().find(|n| n.id == target);
        assert!(target_node.is_some());
        
        let target = target_node.unwrap();
        // Both anchors contribute to target's activation
        assert!(target.spread_score > 0.0);
    }

    // ========================================================================
    // TEST 9: Structural score based on in-degree centrality
    // ========================================================================
    #[test]
    fn test_structural_score_centrality() {
        let config = test_config();
        let target = Uuid::new_v4();
        let source1 = Uuid::new_v4();
        let source2 = Uuid::new_v4();
        let source3 = Uuid::new_v4();

        // Target has 3 incoming edges (high centrality)
        let anchors = vec![make_anchor(source1, "episode", 0.5)];
        let edges = vec![
            make_edge(source1, target, "fact", 0.5),
            make_edge(source2, target, "fact", 0.5),
            make_edge(source3, target, "fact", 0.5),
        ];

        let result = spread_activation_core(&anchors, &edges, &config);

        // Target should have structural score = 3/3 = 1.0
        let target_node = result.nodes.iter().find(|n| n.id == target).unwrap();
        assert!((target_node.structural_score - 1.0).abs() < 0.01);
    }
}
