use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// CLI arguments structure using clap
#[derive(Parser, Debug)]
#[command(name = "whichport")]
#[command(about = "Query listening TCP ports and their processes", long_about = None)]
struct Cli {
    /// Port numbers to query (1-65535)
    #[arg(value_parser = parse_port)]
    ports: Vec<u16>,

    /// Query all listening ports
    #[arg(long)]
    all: bool,

    /// Output in JSON format
    #[arg(long)]
    json: bool,

    /// Include metadata in text output
    #[arg(long)]
    verbose: bool,
}

/// Parse and validate port number
fn parse_port(s: &str) -> Result<u16, String> {
    let port = s
        .parse::<u16>()
        .map_err(|_| format!("invalid port: {s}"))?;
    // Port 0 is technically valid as a u16 but is reserved and shouldn't be queried
    if port == 0 {
        return Err("port 0 is reserved and cannot be queried".to_string());
    }
    Ok(port)
}

/// Custom error type for whichport operations
#[derive(Error, Debug)]
enum WhichportError {
    #[error("no ports specified and --all not provided")]
    NoPorts,

    #[error("failed to run {command}: {details}")]
    CommandFailed { command: String, details: String },

    #[error("command {command} returned error: {stderr}")]
    CommandError { command: String, stderr: String },

    #[error("all collection methods failed: {0}")]
    AllMethodsFailed(String),
}

/// Individual listener entry
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
struct Listener {
    port: u16,
    pid: Option<u32>,
    command: String,
    user: String,
    endpoint: String,
}

/// Aggregated listener with multiple endpoints
#[derive(Debug, Clone, Serialize)]
struct AggregatedListener {
    port: u16,
    pid: Option<u32>,
    command: String,
    user: String,
    /// Primary endpoint for backward compatibility
    endpoint: String,
    /// All endpoints for this listener
    endpoints: Vec<String>,
    /// Inferred role information
    role: Role,
}

/// Collection result with metadata
#[derive(Debug)]
struct CollectionResult {
    listeners: Vec<Listener>,
    source: &'static str,
    errors: Vec<String>,
}

/// Role inference result
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct Role {
    description: &'static str,
    confidence: &'static str,
}

/// JSON output structure for port query mode
#[derive(Debug, Serialize)]
struct PortQueryOutput {
    mode: String,
    source: String,
    timestamp: u64,
    errors: Vec<String>,
    results: Vec<PortResult>,
}

/// Individual port result
#[derive(Debug, Serialize)]
struct PortResult {
    port: u16,
    listening: bool,
    listeners: Vec<AggregatedListener>,
}

/// JSON output structure for all ports mode
#[derive(Debug, Serialize)]
struct AllPortsOutput {
    mode: String,
    source: String,
    timestamp: u64,
    errors: Vec<String>,
    results: Vec<AggregatedListener>,
}

/// Role inference rule
struct RoleRule {
    command_pattern: &'static str,
    description: &'static str,
    confidence: &'static str,
}

/// Common lsof arguments
const LSOF_ARGS: &[&str] = &["-nP", "-iTCP", "-sTCP:LISTEN", "-FpcLnTu"];

/// Role inference rules based on command name
const COMMAND_RULES: &[RoleRule] = &[
    RoleRule {
        command_pattern: "postgres",
        description: "PostgreSQL database",
        confidence: "high",
    },
    RoleRule {
        command_pattern: "redis",
        description: "Redis cache or message broker",
        confidence: "high",
    },
    RoleRule {
        command_pattern: "nginx",
        description: "Web server or reverse proxy",
        confidence: "high",
    },
    RoleRule {
        command_pattern: "docker",
        description: "Container runtime backend",
        confidence: "high",
    },
    RoleRule {
        command_pattern: "ollama",
        description: "Local LLM serving runtime",
        confidence: "high",
    },
    RoleRule {
        command_pattern: "rustrover",
        description: "IDE or developer tooling service",
        confidence: "medium",
    },
    RoleRule {
        command_pattern: "jetbrains",
        description: "IDE or developer tooling service",
        confidence: "medium",
    },
    RoleRule {
        command_pattern: "toolbox",
        description: "IDE or developer tooling service",
        confidence: "medium",
    },
    RoleRule {
        command_pattern: "raycast",
        description: "Productivity launcher local service",
        confidence: "medium",
    },
    RoleRule {
        command_pattern: "adobe",
        description: "Adobe desktop background service",
        confidence: "medium",
    },
    RoleRule {
        command_pattern: "node",
        description: "Node.js application server",
        confidence: "medium",
    },
];

/// Port-based role inference rules
const PORT_RULES: &[(u16, &str, &str)] = &[
    (22, "SSH service", "medium"),
    (80, "HTTP web service", "medium"),
    (443, "HTTPS web service", "medium"),
    (3306, "MySQL database", "medium"),
    (5432, "PostgreSQL database", "medium"),
    (6379, "Redis cache or message broker", "medium"),
];

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), WhichportError> {
    let cli = Cli::parse();

    // Validate that we have either ports or --all
    if !cli.all && cli.ports.is_empty() {
        return Err(WhichportError::NoPorts);
    }

    let collected = collect_listeners()?;
    let timestamp = unix_timestamp();

    if cli.all {
        if cli.json {
            print_all_json(
                &collected.listeners,
                collected.source,
                timestamp,
                &collected.errors,
            );
        } else {
            print_all_text(
                &collected.listeners,
                collected.source,
                timestamp,
                &collected.errors,
                cli.verbose,
            );
        }
        return Ok(());
    }

    if cli.json {
        print_ports_json(
            &collected.listeners,
            &cli.ports,
            collected.source,
            timestamp,
            &collected.errors,
        );
    } else {
        print_ports_text(
            &collected.listeners,
            &cli.ports,
            collected.source,
            timestamp,
            &collected.errors,
            cli.verbose,
        );
    }

    Ok(())
}

/// Collect listening ports using platform-appropriate methods
fn collect_listeners() -> Result<CollectionResult, WhichportError> {
    #[cfg(target_os = "linux")]
    {
        let mut errors = Vec::new();

        // Try ss first on Linux
        match collect_listeners_from_ss() {
            Ok(listeners) => {
                return Ok(CollectionResult {
                    listeners,
                    source: "ss",
                    errors,
                });
            }
            Err(err) => errors.push(err.to_string()),
        }

        // Fallback to lsof
        match collect_listeners_from_lsof() {
            Ok(listeners) => {
                return Ok(CollectionResult {
                    listeners,
                    source: "lsof",
                    errors,
                });
            }
            Err(err) => errors.push(err.to_string()),
        }

        Err(WhichportError::AllMethodsFailed(errors.join(" | ")))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let listeners = collect_listeners_from_lsof()?;
        Ok(CollectionResult {
            listeners,
            source: "lsof",
            errors: Vec::new(),
        })
    }
}

/// Collect listeners using lsof command
fn collect_listeners_from_lsof() -> Result<Vec<Listener>, WhichportError> {
    let output = Command::new("lsof")
        .args(LSOF_ARGS)
        .output()
        .map_err(|e| WhichportError::CommandFailed {
            command: "lsof".to_string(),
            details: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WhichportError::CommandError {
            command: "lsof".to_string(),
            stderr: stderr.trim().to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_lsof_output(&stdout))
}

/// Collect listeners using ss command (Linux only)
#[cfg(target_os = "linux")]
fn collect_listeners_from_ss() -> Result<Vec<Listener>, WhichportError> {
    let output = Command::new("ss")
        .args(["-lntpH"])
        .output()
        .map_err(|e| WhichportError::CommandFailed {
            command: "ss".to_string(),
            details: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WhichportError::CommandError {
            command: "ss".to_string(),
            stderr: stderr.trim().to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_ss_output(&stdout))
}

/// Parse lsof -F output format
fn parse_lsof_output(raw: &str) -> Vec<Listener> {
    let mut current_pid: Option<u32> = None;
    let mut current_command: Option<String> = None;
    let mut current_user: Option<String> = None;

    let mut out = Vec::new();
    let mut dedup = HashSet::new();

    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }

        let (tag, value) = line.split_at(1);
        match tag {
            "p" => current_pid = value.parse::<u32>().ok(),
            "c" => current_command = Some(value.to_string()),
            "L" => current_user = Some(value.to_string()),
            "u" if current_user.is_none() => current_user = Some(value.to_string()),
            "n" => {
                let port = match parse_port_from_endpoint(value) {
                    Some(port) => port,
                    None => continue,
                };

                if let (Some(command), Some(user)) =
                    (current_command.as_ref(), current_user.as_ref())
                {
                    let record = Listener {
                        port,
                        pid: current_pid,
                        command: command.clone(),
                        user: user.clone(),
                        endpoint: value.to_string(),
                    };

                    if dedup.insert(record.clone()) {
                        out.push(record);
                    }
                }
            }
            _ => {}
        }
    }

    out.sort_by_key(|l| (l.port, l.pid.unwrap_or(0)));
    out
}

/// Parse ss output format (Linux)
#[cfg(any(target_os = "linux", test))]
fn parse_ss_output(raw: &str) -> Vec<Listener> {
    let mut out = Vec::new();
    let mut dedup = HashSet::new();

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            continue;
        }

        let endpoint = tokens[3];
        let port = match parse_port_from_endpoint(endpoint) {
            Some(port) => port,
            None => continue,
        };

        let proc_blob = if tokens.len() > 5 {
            tokens[5..].join(" ")
        } else {
            String::new()
        };

        let (pid, command) = parse_ss_process_info(&proc_blob);
        let record = Listener {
            port,
            pid,
            command,
            user: "-".to_string(),
            endpoint: endpoint.to_string(),
        };

        if dedup.insert(record.clone()) {
            out.push(record);
        }
    }

    out.sort_by_key(|l| (l.port, l.pid.unwrap_or(0)));
    out
}

/// Parse process information from ss output
#[cfg(any(target_os = "linux", test))]
fn parse_ss_process_info(raw: &str) -> (Option<u32>, String) {
    let mut command = "unknown".to_string();
    let mut pid = None;

    // Extract command name from quoted string
    if let Some(start) = raw.find('"') {
        let remain = &raw[start + 1..];
        if let Some(end) = remain.find('"') {
            command = remain[..end].to_string();
        }
    }

    // Extract PID from pid= pattern
    if let Some(idx) = raw.find("pid=") {
        let digits: String = raw[idx + 4..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        pid = digits.parse::<u32>().ok();
    }

    (pid, command)
}

/// Extract port number from endpoint string
fn parse_port_from_endpoint(endpoint: &str) -> Option<u16> {
    if let Some(idx) = endpoint.rfind(':') {
        let port_str = &endpoint[idx + 1..];
        return port_str.parse::<u16>().ok();
    }
    None
}

/// Infer the role of a service based on port and command name
fn infer_role(port: u16, command: &str) -> Role {
    let cmd = command.to_ascii_lowercase();

    // Check command-based rules first (higher priority)
    for rule in COMMAND_RULES {
        if cmd.contains(rule.command_pattern) {
            return Role {
                description: rule.description,
                confidence: rule.confidence,
            };
        }
    }

    // Check port-based rules
    for &(rule_port, description, confidence) in PORT_RULES {
        if port == rule_port {
            return Role {
                description,
                confidence,
            };
        }
    }

    // Default fallback
    Role {
        description: "Unknown application service",
        confidence: "medium",
    }
}

/// Aggregate listeners by (port, pid, command, user) and merge endpoints
fn aggregate_listeners(listeners: &[Listener]) -> Vec<AggregatedListener> {
    let mut grouped: BTreeMap<(u16, Option<u32>, String, String), BTreeSet<String>> =
        BTreeMap::new();

    for listener in listeners {
        grouped
            .entry((
                listener.port,
                listener.pid,
                listener.command.clone(),
                listener.user.clone(),
            ))
            .or_default()
            .insert(listener.endpoint.clone());
    }

    grouped
        .into_iter()
        .map(
            |((port, pid, command, user), endpoints)| {
                let endpoints_vec: Vec<String> = endpoints.into_iter().collect();
                let primary_endpoint = endpoints_vec.first().cloned().unwrap_or_default();
                let role = infer_role(port, &command);

                AggregatedListener {
                    port,
                    pid,
                    command,
                    user,
                    endpoint: primary_endpoint,
                    endpoints: endpoints_vec,
                    role,
                }
            },
        )
        .collect()
}

/// Print results for specific ports in text format
fn print_ports_text(
    listeners: &[Listener],
    ports: &[u16],
    source: &str,
    timestamp: u64,
    errors: &[String],
    verbose: bool,
) {
    print_text_meta(source, timestamp, errors, verbose);
    let aggregated = aggregate_listeners(listeners);

    for &port in ports {
        let matches: Vec<&AggregatedListener> =
            aggregated.iter().filter(|l| l.port == port).collect();
        if matches.is_empty() {
            println!("port {port}: not listening");
            continue;
        }

        for listener in matches {
            print_listener_text(listener);
        }
    }
}

/// Print all listening ports in text format
fn print_all_text(
    listeners: &[Listener],
    source: &str,
    timestamp: u64,
    errors: &[String],
    verbose: bool,
) {
    print_text_meta(source, timestamp, errors, verbose);
    let aggregated = aggregate_listeners(listeners);

    if aggregated.is_empty() {
        println!("no listening ports found");
        return;
    }

    for listener in &aggregated {
        print_listener_text(listener);
    }
}

/// Print a single listener in text format
fn print_listener_text(listener: &AggregatedListener) {
    let endpoints = listener.endpoints.join(", ");
    println!(
        "port {}: {} (pid {}, user {}) on [{}] | {} ({})",
        listener.port,
        listener.command,
        pid_display(listener.pid),
        listener.user,
        endpoints,
        listener.role.description,
        listener.role.confidence
    );
}

/// Print metadata in text format if verbose is enabled
fn print_text_meta(source: &str, timestamp: u64, errors: &[String], verbose: bool) {
    if !verbose {
        return;
    }

    for line in build_text_meta_lines(source, timestamp, errors) {
        println!("{line}");
    }
}

/// Build metadata lines for text output
fn build_text_meta_lines(source: &str, timestamp: u64, errors: &[String]) -> Vec<String> {
    let mut lines = Vec::with_capacity(3 + errors.len());
    lines.push(format!("meta source: {source}"));
    lines.push(format!("meta timestamp: {timestamp}"));
    lines.push(format!("meta errors: {}", errors.len()));
    for err in errors {
        lines.push(format!("meta error: {err}"));
    }
    lines
}

/// Print all listening ports in JSON format
fn print_all_json(listeners: &[Listener], source: &str, timestamp: u64, errors: &[String]) {
    let aggregated = aggregate_listeners(listeners);
    let output = AllPortsOutput {
        mode: "all".to_string(),
        source: source.to_string(),
        timestamp,
        errors: errors.to_vec(),
        results: aggregated,
    };

    // Use serde_json for safe and correct JSON serialization
    match serde_json::to_string(&output) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("error: failed to serialize JSON: {e}"),
    }
}

/// Print results for specific ports in JSON format
fn print_ports_json(
    listeners: &[Listener],
    ports: &[u16],
    source: &str,
    timestamp: u64,
    errors: &[String],
) {
    let aggregated = aggregate_listeners(listeners);
    let results: Vec<PortResult> = ports
        .iter()
        .map(|&port| {
            let matches: Vec<AggregatedListener> = aggregated
                .iter()
                .filter(|l| l.port == port)
                .cloned()
                .collect();
            PortResult {
                port,
                listening: !matches.is_empty(),
                listeners: matches,
            }
        })
        .collect();

    let output = PortQueryOutput {
        mode: "ports".to_string(),
        source: source.to_string(),
        timestamp,
        errors: errors.to_vec(),
        results,
    };

    match serde_json::to_string(&output) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("error: failed to serialize JSON: {e}"),
    }
}

/// Get current Unix timestamp
fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Display PID as string, "unknown" if None
fn pid_display(pid: Option<u32>) -> String {
    pid.map_or_else(|| "unknown".to_string(), |v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_from_endpoint_ipv4() {
        assert_eq!(parse_port_from_endpoint("*:8080"), Some(8080));
    }

    #[test]
    fn test_parse_port_from_endpoint_ipv6() {
        assert_eq!(parse_port_from_endpoint("[::1]:5432"), Some(5432));
    }

    #[test]
    fn test_parse_port_from_endpoint_invalid() {
        assert_eq!(parse_port_from_endpoint("localhost"), None);
    }

    #[test]
    fn test_parse_ss_process_info_complete() {
        let raw = "users:((\"postgres\",pid=1178,fd=7))";
        let (pid, command) = parse_ss_process_info(raw);
        assert_eq!(pid, Some(1178));
        assert_eq!(command, "postgres");
    }

    #[test]
    fn test_parse_ss_process_info_missing() {
        let (pid, command) = parse_ss_process_info("");
        assert_eq!(pid, None);
        assert_eq!(command, "unknown");
    }

    #[test]
    fn test_parse_ss_output_variants() {
        let raw = concat!(
            "LISTEN 0 128 *:22 *:*\n",
            "LISTEN 0 4096 127.0.0.53%lo:53 0.0.0.0:* users:((\"systemd-resolve\",pid=728,fd=14))\n",
            "LISTEN 0 511 [::]:443 [::]:* users:((\"nginx\",pid=1000,fd=7))\n"
        );
        let parsed = parse_ss_output(raw);

        assert_eq!(parsed.len(), 3);
        assert!(parsed.iter().any(|v| v.port == 22 && v.pid.is_none()));
        assert!(parsed
            .iter()
            .any(|v| v.port == 53 && v.pid == Some(728) && v.command == "systemd-resolve"));
        assert!(parsed
            .iter()
            .any(|v| v.port == 443 && v.pid == Some(1000) && v.command == "nginx"));
    }

    #[test]
    fn test_build_text_meta_lines_includes_errors() {
        let errors = vec!["fallback: ss failed".to_string(), "lsof warning".to_string()];
        let lines = build_text_meta_lines("lsof", 1700000000, &errors);

        assert_eq!(lines[0], "meta source: lsof");
        assert_eq!(lines[1], "meta timestamp: 1700000000");
        assert_eq!(lines[2], "meta errors: 2");
        assert_eq!(lines[3], "meta error: fallback: ss failed");
        assert_eq!(lines[4], "meta error: lsof warning");
    }

    #[test]
    fn test_aggregate_listeners_merges_endpoints() {
        let listeners = vec![
            Listener {
                port: 80,
                pid: Some(10),
                command: "nginx".to_string(),
                user: "root".to_string(),
                endpoint: "*:80".to_string(),
            },
            Listener {
                port: 80,
                pid: Some(10),
                command: "nginx".to_string(),
                user: "root".to_string(),
                endpoint: "[::]:80".to_string(),
            },
        ];

        let aggregated = aggregate_listeners(&listeners);
        assert_eq!(aggregated.len(), 1);
        assert_eq!(aggregated[0].port, 80);
        assert_eq!(aggregated[0].pid, Some(10));
        assert_eq!(
            aggregated[0].endpoints,
            vec!["*:80".to_string(), "[::]:80".to_string()]
        );
    }

    #[test]
    fn test_infer_role_by_command_postgres() {
        let role = infer_role(9999, "postgres");
        assert_eq!(role.description, "PostgreSQL database");
        assert_eq!(role.confidence, "high");
    }

    #[test]
    fn test_infer_role_by_command_redis() {
        let role = infer_role(9999, "redis-server");
        assert_eq!(role.description, "Redis cache or message broker");
        assert_eq!(role.confidence, "high");
    }

    #[test]
    fn test_infer_role_by_port_ssh() {
        let role = infer_role(22, "sshd");
        assert_eq!(role.description, "SSH service");
        assert_eq!(role.confidence, "medium");
    }

    #[test]
    fn test_infer_role_by_port_http() {
        let role = infer_role(80, "httpd");
        assert_eq!(role.description, "HTTP web service");
        assert_eq!(role.confidence, "medium");
    }

    #[test]
    fn test_infer_role_unknown() {
        let role = infer_role(9999, "myapp");
        assert_eq!(role.description, "Unknown application service");
        assert_eq!(role.confidence, "medium");
    }

    #[test]
    fn test_parse_lsof_output_complete() {
        let raw = "p123\ncpostgres\nLrexfelix\nn127.0.0.1:5432\nn[::1]:5432\n";
        let parsed = parse_lsof_output(raw);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].port, 5432);
        assert_eq!(parsed[0].pid, Some(123));
        assert_eq!(parsed[0].command, "postgres");
        assert_eq!(parsed[0].user, "rexfelix");
        assert_eq!(parsed[0].endpoint, "127.0.0.1:5432");
    }

    #[test]
    fn test_parse_lsof_output_with_user_fallback() {
        let raw = "p456\ncnginx\nu0\nn*:80\n";
        let parsed = parse_lsof_output(raw);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].port, 80);
        assert_eq!(parsed[0].user, "0");
    }

    #[test]
    fn test_pid_display_some() {
        assert_eq!(pid_display(Some(123)), "123");
    }

    #[test]
    fn test_pid_display_none() {
        assert_eq!(pid_display(None), "unknown");
    }

    #[test]
    fn test_parse_port_valid() {
        assert_eq!(parse_port("8080").unwrap(), 8080);
    }

    #[test]
    fn test_parse_port_zero() {
        assert!(parse_port("0").is_err());
    }

    #[test]
    fn test_parse_port_invalid() {
        assert!(parse_port("abc").is_err());
    }

    #[test]
    fn test_parse_port_overflow() {
        assert!(parse_port("99999").is_err());
    }
}
