//! ethos-cli — QMD wire-protocol-compatible CLI for Ethos semantic memory search
//!
//! Drop-in replacement for the `qmd` binary. OpenClaw's `memory_search` tool calls
//! `ethos-cli search <query> -n <limit> --json` and parses the stdout as QMD-format JSON.
//!
//! # Subcommands
//! - `search <query> [-n <limit>] [--json]` — semantic search
//! - `query <query> [-n <limit>] [--json]`  — alias for search
//! - `status`                                — show server health

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

const DEFAULT_SERVER: &str = "http://127.0.0.1:8766";
const DEFAULT_LIMIT: usize = 5;

// ============================================================================
// CLI Definition
// ============================================================================

#[derive(Debug, Parser)]
#[command(
    name = "ethos-cli",
    version,
    about = "Ethos semantic memory search — QMD wire-protocol-compatible CLI"
)]
struct Cli {
    /// Ethos HTTP server URL (overrides ETHOS_HTTP_URL env var)
    #[arg(long, env = "ETHOS_HTTP_URL", default_value = DEFAULT_SERVER)]
    server: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Search memory semantically (QMD-compatible)
    Search {
        /// Query text to search for
        query: String,

        /// Maximum number of results to return
        #[arg(short = 'n', long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,

        /// Output results as QMD-compatible JSON array
        #[arg(long)]
        json: bool,

        /// Enable spreading activation for associative retrieval
        #[arg(long)]
        spreading: bool,
    },

    /// Query memory semantically (alias for search)
    Query {
        /// Query text to search for
        query: String,

        /// Maximum number of results to return
        #[arg(short = 'n', long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,

        /// Output results as QMD-compatible JSON array
        #[arg(long)]
        json: bool,

        /// Enable spreading activation for associative retrieval
        #[arg(long)]
        spreading: bool,
    },

    /// Show Ethos server status
    Status,
}

// ============================================================================
// API Response Types
// ============================================================================

/// A single memory result from the Ethos HTTP API
#[derive(Debug, Deserialize)]
pub struct EthosSearchResult {
    pub id: String,
    pub content: String,
    pub score: f64,
    pub source: String,
    pub created_at: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// The full search response from POST /search
#[derive(Debug, Deserialize)]
pub struct EthosSearchResponse {
    pub results: Vec<EthosSearchResult>,
    pub query: String,
    pub count: usize,
    pub took_ms: Option<u64>,
}

// ============================================================================
// QMD Output Format
// ============================================================================

/// QMD-compatible result JSON format.
///
/// OpenClaw's QMD manager parses stdout from `qmd search <query> --json`
/// as an array of these objects.
#[derive(Debug, Serialize)]
pub struct QmdResult {
    /// Short document ID: "#" followed by first 6 hex chars of UUID (no dashes)
    pub docid: String,
    /// Similarity score 0.0–1.0
    pub score: f64,
    /// Source URI: "ethos://memory/{uuid}"
    pub file: String,
    /// First line of content, truncated to 60 characters
    pub title: String,
    /// Diff-header snippet: "@@ -1,4 @@\n\n{content truncated to 300 chars}"
    pub snippet: String,
}

/// Convert an Ethos search result to QMD wire format.
pub fn to_qmd_result(r: &EthosSearchResult) -> QmdResult {
    // docid: "#" + first 6 hex chars of UUID (dashes removed)
    let uuid_hex = r.id.replace('-', "");
    let docid = format!("#{}", &uuid_hex[..6.min(uuid_hex.len())]);

    // file: ethos://memory/{uuid}
    let file = format!("ethos://memory/{}", r.id);

    // title: first non-empty line of content, capped at 60 chars
    let title: String = r
        .content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(60)
        .collect();

    // snippet: QMD diff-header format + truncated content
    let content_preview: String = r.content.chars().take(300).collect();
    let snippet = format!("@@ -1,4 @@\n\n{}", content_preview);

    QmdResult {
        docid,
        score: r.score,
        file,
        title,
        snippet,
    }
}

// ============================================================================
// HTTP Client Calls
// ============================================================================

/// Perform a semantic search against the Ethos HTTP API.
fn do_search(
    server: &str,
    query: &str,
    limit: usize,
    json_output: bool,
    use_spreading: bool,
) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let url = format!("{}/search", server);
    let body = serde_json::json!({
        "query": query,
        "limit": limit,
        "use_spreading": use_spreading,
    });

    let resp = client.post(&url).json(&body).send();

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ethos-cli: connection failed to {}: {}", url, e);
            std::process::exit(1);
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        eprintln!("ethos-cli: server returned {}: {}", status, body);
        std::process::exit(1);
    }

    let search_resp: EthosSearchResponse = match resp.json() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ethos-cli: failed to parse search response: {}", e);
            std::process::exit(1);
        }
    };

    if json_output {
        // QMD-compatible JSON array output
        let qmd_results: Vec<QmdResult> = search_resp.results.iter().map(to_qmd_result).collect();
        match serde_json::to_string_pretty(&qmd_results) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("ethos-cli: failed to serialize results: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        // Human-readable format (mirrors QMD text output)
        if search_resp.results.is_empty() {
            eprintln!("No results found for: {}", query);
            return Ok(());
        }
        for r in &search_resp.results {
            let uuid_hex = r.id.replace('-', "");
            println!(
                "ethos://memory/{} #{}",
                r.id,
                &uuid_hex[..6.min(uuid_hex.len())]
            );
            println!("Score:  {:.0}%\n", r.score * 100.0);
            let preview: String = r.content.chars().take(200).collect();
            println!("{}\n", preview);
        }
    }

    Ok(())
}

/// Show the server status by calling GET /health.
fn do_status(server: &str) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let url = format!("{}/health", server);
    let resp = client.get(&url).send();

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().unwrap_or_default();
            println!("Ethos server: {}", body["status"].as_str().unwrap_or("unknown"));
            println!("Version:      {}", body["version"].as_str().unwrap_or("?"));
            println!("PostgreSQL:   {}", body["postgresql"].as_str().unwrap_or("?"));
            println!("pgvector:     {}", body["pgvector"].as_str().unwrap_or("?"));
            println!("Socket:       {}", body["socket"].as_str().unwrap_or("?"));
        }
        Ok(r) => {
            let status = r.status();
            eprintln!("ethos-cli: server unhealthy (HTTP {})", status);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("ethos-cli: cannot reach {} — {}", url, e);
            std::process::exit(1);
        }
    }

    Ok(())
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    let cli = Cli::parse();
    let server = cli.server.trim_end_matches('/').to_string();

    let result = match cli.command {
        Commands::Search { query, limit, json, spreading }
        | Commands::Query { query, limit, json, spreading } => {
            do_search(&server, &query, limit, json, spreading)
        }
        Commands::Status => do_status(&server),
    };

    if let Err(e) = result {
        eprintln!("ethos-cli: {}", e);
        std::process::exit(1);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a mock EthosSearchResult for testing
    fn mock_result(id: &str, content: &str, score: f64) -> EthosSearchResult {
        EthosSearchResult {
            id: id.to_string(),
            content: content.to_string(),
            score,
            source: "user".to_string(),
            created_at: Some("2026-02-23T10:00:00Z".to_string()),
            metadata: None,
        }
    }

    // ========================================================================
    // TEST 1: QMD docid format — starts with "#", 7 chars total
    // ========================================================================
    #[test]
    fn test_qmd_docid_format() {
        let result = mock_result(
            "7b5c24ab-1234-5678-9abc-def012345678",
            "Some content here",
            0.87,
        );
        let qmd = to_qmd_result(&result);

        assert!(qmd.docid.starts_with('#'), "docid must start with '#'");
        // "#" + 6 hex chars = 7 chars total
        assert_eq!(qmd.docid.len(), 7, "docid should be '#' + 6 hex chars");
        // The 6 chars after '#' come from UUID with dashes removed
        let uuid_hex = result.id.replace('-', "");
        assert_eq!(&qmd.docid[1..], &uuid_hex[..6]);
    }

    // ========================================================================
    // TEST 2: QMD file format — starts with "ethos://memory/"
    // ========================================================================
    #[test]
    fn test_qmd_file_format() {
        let id = "7b5c24ab-1234-5678-9abc-def012345678";
        let result = mock_result(id, "Some content", 0.5);
        let qmd = to_qmd_result(&result);

        assert!(
            qmd.file.starts_with("ethos://memory/"),
            "file must start with 'ethos://memory/', got: {}",
            qmd.file
        );
        assert!(
            qmd.file.ends_with(id),
            "file must end with the UUID, got: {}",
            qmd.file
        );
    }

    // ========================================================================
    // TEST 3: QMD snippet format — starts with "@@ -1,4 @@"
    // ========================================================================
    #[test]
    fn test_qmd_snippet_format() {
        let result = mock_result(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "This is the content of the memory node",
            0.75,
        );
        let qmd = to_qmd_result(&result);

        assert!(
            qmd.snippet.starts_with("@@ -1,4 @@"),
            "snippet must start with '@@ -1,4 @@', got: {}",
            qmd.snippet
        );
    }

    // ========================================================================
    // TEST 4: QMD title — first line, truncated to 60 chars
    // ========================================================================
    #[test]
    fn test_qmd_title_truncation() {
        let long_first_line = "A".repeat(100);
        let content = format!("{}\nSecond line here", long_first_line);
        let result = mock_result("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", &content, 0.5);
        let qmd = to_qmd_result(&result);

        assert!(
            qmd.title.len() <= 60,
            "title should be truncated to 60 chars, got {}",
            qmd.title.len()
        );
        assert_eq!(qmd.title, "A".repeat(60));
    }

    // ========================================================================
    // TEST 5: QMD snippet — content truncated to 300 chars
    // ========================================================================
    #[test]
    fn test_qmd_snippet_content_truncation() {
        let long_content = "B".repeat(500);
        let result = mock_result(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            &long_content,
            0.5,
        );
        let qmd = to_qmd_result(&result);

        // snippet = "@@ -1,4 @@\n\n" + content[..300]
        let header = "@@ -1,4 @@\n\n";
        assert!(qmd.snippet.starts_with(header));
        let content_part = &qmd.snippet[header.len()..];
        assert_eq!(
            content_part.len(),
            300,
            "Content part of snippet should be 300 chars"
        );
    }

    // ========================================================================
    // TEST 6: QMD output serialises correctly as JSON array
    // ========================================================================
    #[test]
    fn test_qmd_json_array_serialization() {
        let results = vec![
            mock_result("7b5c24ab-1234-5678-9abc-def012345678", "First memory", 0.9),
            mock_result(
                "deadbeef-cafe-babe-face-feeddeadbeef",
                "Second memory",
                0.7,
            ),
        ];

        let qmd_results: Vec<QmdResult> = results.iter().map(to_qmd_result).collect();
        let json = serde_json::to_string(&qmd_results).expect("Should serialize");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&json).expect("Should parse back");

        assert_eq!(parsed.len(), 2);

        // Verify first result has all required QMD fields
        let first = &parsed[0];
        assert!(first["docid"].is_string());
        assert!(first["score"].is_number());
        assert!(first["file"].is_string());
        assert!(first["title"].is_string());
        assert!(first["snippet"].is_string());

        assert!(
            first["docid"].as_str().unwrap().starts_with('#'),
            "docid must start with '#'"
        );
        assert!(
            first["file"]
                .as_str()
                .unwrap()
                .starts_with("ethos://memory/"),
            "file must start with 'ethos://memory/'"
        );
        assert!(
            first["snippet"]
                .as_str()
                .unwrap()
                .starts_with("@@ -1,4 @@"),
            "snippet must start with '@@ -1,4 @@'"
        );
    }

    // ========================================================================
    // TEST 7: Empty content handled gracefully
    // ========================================================================
    #[test]
    fn test_qmd_empty_content_graceful() {
        let result = mock_result("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "", 0.5);
        let qmd = to_qmd_result(&result);

        assert!(qmd.title.is_empty(), "Empty content should produce empty title");
        assert!(
            qmd.snippet.starts_with("@@ -1,4 @@"),
            "Should still have snippet header"
        );
    }

    // ========================================================================
    // TEST 8: to_qmd_result preserves score exactly
    // ========================================================================
    #[test]
    fn test_qmd_score_preserved() {
        let result = mock_result(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "content",
            0.87654321,
        );
        let qmd = to_qmd_result(&result);
        assert!(
            (qmd.score - 0.87654321).abs() < f64::EPSILON,
            "Score should be preserved exactly"
        );
    }

    // ========================================================================
    // TEST 9: UUID without dashes still produces valid docid
    // ========================================================================
    #[test]
    fn test_qmd_uuid_without_dashes() {
        // Some edge case where UUID might come through without dashes
        let result = mock_result("aabbccddeeff11223344556677889900", "content", 0.5);
        let qmd = to_qmd_result(&result);

        assert!(qmd.docid.starts_with('#'));
        assert!(qmd.docid.len() >= 2, "docid should have at least # + 1 char");
    }

    // ========================================================================
    // TEST 10: Multiline content — title uses first non-empty line
    // ========================================================================
    #[test]
    fn test_qmd_title_uses_first_nonempty_line() {
        let content = "\n\nFirst real line\nSecond line";
        let result = mock_result(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            content,
            0.5,
        );
        let qmd = to_qmd_result(&result);
        assert_eq!(qmd.title, "First real line");
    }
}
