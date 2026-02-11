use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::env;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct Listener {
    port: u16,
    pid: Option<u32>,
    command: String,
    user: String,
    endpoint: String,
}

#[derive(Debug, Clone)]
struct AggregatedListener {
    port: u16,
    pid: Option<u32>,
    command: String,
    user: String,
    endpoints: Vec<String>,
}

#[derive(Debug)]
struct Cli {
    ports: Vec<u16>,
    all: bool,
    json: bool,
    verbose: bool,
}

#[derive(Debug)]
struct CollectionResult {
    listeners: Vec<Listener>,
    source: &'static str,
    errors: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct Role {
    description: &'static str,
    confidence: &'static str,
}

fn main() {
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let cli = parse_cli()?;
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

fn parse_cli() -> Result<Cli, String> {
    let mut ports = Vec::new();
    let mut all = false;
    let mut json = false;
    let mut verbose = false;

    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return Err(help_text());
    }

    for arg in args {
        match arg.as_str() {
            "--all" => all = true,
            "--json" => json = true,
            "--verbose" => verbose = true,
            "-h" | "--help" => return Err(help_text()),
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option: {arg}\n\n{}", help_text()));
            }
            _ => {
                let port = arg
                    .parse::<u16>()
                    .map_err(|_| format!("invalid port: {arg}"))?;
                if port == 0 {
                    return Err(format!("invalid port: {arg}"));
                }
                ports.push(port);
            }
        }
    }

    if !all && ports.is_empty() {
        return Err(help_text());
    }

    Ok(Cli {
        ports,
        all,
        json,
        verbose,
    })
}

fn help_text() -> String {
    "usage:\n  whichport <port...> [--json] [--verbose]\n  whichport --all [--json] [--verbose]"
        .to_string()
}

fn collect_listeners() -> Result<CollectionResult, String> {
    #[cfg(target_os = "linux")]
    {
        let mut errors = Vec::new();

        match collect_listeners_from_ss() {
            Ok(listeners) => {
                return Ok(CollectionResult {
                    listeners,
                    source: "ss",
                    errors,
                });
            }
            Err(err) => errors.push(err),
        }

        match collect_listeners_from_lsof() {
            Ok(listeners) => {
                return Ok(CollectionResult {
                    listeners,
                    source: "lsof",
                    errors,
                });
            }
            Err(err) => errors.push(err),
        }

        Err(errors.join(" | "))
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

fn collect_listeners_from_lsof() -> Result<Vec<Listener>, String> {
    let output = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-FpcLnTu"])
        .output()
        .map_err(|e| format!("failed to run lsof: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("lsof failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_lsof_output(&stdout))
}

#[cfg(target_os = "linux")]
fn collect_listeners_from_ss() -> Result<Vec<Listener>, String> {
    let output = Command::new("ss")
        .args(["-lntpH"])
        .output()
        .map_err(|e| format!("failed to run ss: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ss failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_ss_output(&stdout))
}

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

#[cfg(any(target_os = "linux", test))]
fn parse_ss_process_info(raw: &str) -> (Option<u32>, String) {
    let mut command = "unknown".to_string();
    let mut pid = None;

    if let Some(start) = raw.find('"') {
        let remain = &raw[start + 1..];
        if let Some(end) = remain.find('"') {
            command = remain[..end].to_string();
        }
    }

    if let Some(idx) = raw.find("pid=") {
        let digits: String = raw[idx + 4..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        pid = digits.parse::<u32>().ok();
    }

    (pid, command)
}

fn parse_port_from_endpoint(endpoint: &str) -> Option<u16> {
    if let Some(idx) = endpoint.rfind(':') {
        let port_str = &endpoint[idx + 1..];
        return port_str.parse::<u16>().ok();
    }
    None
}

fn infer_role(port: u16, command: &str) -> Role {
    let cmd = command.to_ascii_lowercase();

    if cmd.contains("postgres") {
        return Role {
            description: "PostgreSQL database",
            confidence: "high",
        };
    }
    if cmd.contains("redis") {
        return Role {
            description: "Redis cache or message broker",
            confidence: "high",
        };
    }
    if cmd.contains("nginx") {
        return Role {
            description: "Web server or reverse proxy",
            confidence: "high",
        };
    }
    if cmd.contains("docker") {
        return Role {
            description: "Container runtime backend",
            confidence: "high",
        };
    }
    if cmd.contains("ollama") {
        return Role {
            description: "Local LLM serving runtime",
            confidence: "high",
        };
    }
    if cmd.contains("rustrover") || cmd.contains("jetbrains") || cmd.contains("toolbox") {
        return Role {
            description: "IDE or developer tooling service",
            confidence: "medium",
        };
    }
    if cmd.contains("raycast") {
        return Role {
            description: "Productivity launcher local service",
            confidence: "medium",
        };
    }
    if cmd.contains("adobe") {
        return Role {
            description: "Adobe desktop background service",
            confidence: "medium",
        };
    }
    if cmd.contains("node") {
        return Role {
            description: "Node.js application server",
            confidence: "medium",
        };
    }

    match port {
        22 => Role {
            description: "SSH service",
            confidence: "medium",
        },
        80 => Role {
            description: "HTTP web service",
            confidence: "medium",
        },
        443 => Role {
            description: "HTTPS web service",
            confidence: "medium",
        },
        3306 => Role {
            description: "MySQL database",
            confidence: "medium",
        },
        5432 => Role {
            description: "PostgreSQL database",
            confidence: "medium",
        },
        6379 => Role {
            description: "Redis cache or message broker",
            confidence: "medium",
        },
        _ => Role {
            description: "Unknown application service",
            confidence: "medium",
        },
    }
}

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
            |((port, pid, command, user), endpoints)| AggregatedListener {
                port,
                pid,
                command,
                user,
                endpoints: endpoints.into_iter().collect(),
            },
        )
        .collect()
}

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

fn print_listener_text(listener: &AggregatedListener) {
    let role = infer_role(listener.port, &listener.command);
    let endpoints = listener.endpoints.join(", ");
    println!(
        "port {}: {} (pid {}, user {}) on [{}] | {} ({})",
        listener.port,
        listener.command,
        pid_display(listener.pid),
        listener.user,
        endpoints,
        role.description,
        role.confidence
    );
}

fn print_text_meta(source: &str, timestamp: u64, errors: &[String], verbose: bool) {
    if !verbose {
        return;
    }

    for line in build_text_meta_lines(source, timestamp, errors) {
        println!("{line}");
    }
}

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

fn print_all_json(listeners: &[Listener], source: &str, timestamp: u64, errors: &[String]) {
    let aggregated = aggregate_listeners(listeners);
    let mut buf = String::new();
    append_json_header(&mut buf, "all", source, timestamp, errors);
    buf.push_str(",\"results\":[");

    for (i, listener) in aggregated.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        append_aggregated_listener_json(&mut buf, listener);
    }

    buf.push_str("]}");
    println!("{buf}");
}

fn print_ports_json(
    listeners: &[Listener],
    ports: &[u16],
    source: &str,
    timestamp: u64,
    errors: &[String],
) {
    let aggregated = aggregate_listeners(listeners);
    let mut buf = String::new();
    append_json_header(&mut buf, "ports", source, timestamp, errors);
    buf.push_str(",\"results\":[");

    for (i, port) in ports.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }

        let matches: Vec<&AggregatedListener> =
            aggregated.iter().filter(|l| l.port == *port).collect();
        buf.push_str("{\"port\":");
        buf.push_str(&port.to_string());
        buf.push_str(",\"listening\":");
        buf.push_str(if matches.is_empty() { "false" } else { "true" });
        buf.push_str(",\"listeners\":[");

        for (j, listener) in matches.iter().enumerate() {
            if j > 0 {
                buf.push(',');
            }
            append_aggregated_listener_json(&mut buf, listener);
        }

        buf.push_str("]}");
    }

    buf.push_str("]}");
    println!("{buf}");
}

fn append_json_header(buf: &mut String, mode: &str, source: &str, timestamp: u64, errors: &[String]) {
    buf.push_str("{\"mode\":\"");
    buf.push_str(mode);
    buf.push_str("\",\"source\":\"");
    buf.push_str(&escape_json(source));
    buf.push_str("\",\"timestamp\":");
    buf.push_str(&timestamp.to_string());
    buf.push_str(",\"errors\":[");

    for (i, err) in errors.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        buf.push('"');
        buf.push_str(&escape_json(err));
        buf.push('"');
    }

    buf.push(']');
}

fn append_aggregated_listener_json(buf: &mut String, listener: &AggregatedListener) {
    let role = infer_role(listener.port, &listener.command);
    let primary_endpoint = listener
        .endpoints
        .first()
        .map(|v| v.as_str())
        .unwrap_or_default();

    buf.push('{');
    buf.push_str("\"port\":");
    buf.push_str(&listener.port.to_string());
    buf.push_str(",\"pid\":");
    if let Some(pid) = listener.pid {
        buf.push_str(&pid.to_string());
    } else {
        buf.push_str("null");
    }
    buf.push_str(",\"command\":\"");
    buf.push_str(&escape_json(&listener.command));
    buf.push_str("\",\"user\":\"");
    buf.push_str(&escape_json(&listener.user));
    buf.push_str("\",\"endpoint\":\"");
    buf.push_str(&escape_json(primary_endpoint));
    buf.push_str("\",\"endpoints\":[");
    for (i, endpoint) in listener.endpoints.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        buf.push('"');
        buf.push_str(&escape_json(endpoint));
        buf.push('"');
    }
    buf.push_str("],\"role\":{\"description\":\"");
    buf.push_str(&escape_json(role.description));
    buf.push_str("\",\"confidence\":\"");
    buf.push_str(role.confidence);
    buf.push_str("\"}}");
}

fn escape_json(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str("\\u");
                out.push_str(&format!("{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pid_display(pid: Option<u32>) -> String {
    pid.map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_ipv4() {
        assert_eq!(parse_port_from_endpoint("*:8080"), Some(8080));
    }

    #[test]
    fn parse_port_ipv6() {
        assert_eq!(parse_port_from_endpoint("[::1]:5432"), Some(5432));
    }

    #[test]
    fn parse_port_invalid() {
        assert_eq!(parse_port_from_endpoint("localhost"), None);
    }

    #[test]
    fn parse_ss_process() {
        let raw = "users:((\"postgres\",pid=1178,fd=7))";
        let (pid, command) = parse_ss_process_info(raw);
        assert_eq!(pid, Some(1178));
        assert_eq!(command, "postgres");
    }

    #[test]
    fn parse_ss_process_missing() {
        let (pid, command) = parse_ss_process_info("");
        assert_eq!(pid, None);
        assert_eq!(command, "unknown");
    }

    #[test]
    fn parse_ss_output_variants() {
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
    fn escape_json_quote_and_newline() {
        let escaped = escape_json("a\"b\\n");
        assert_eq!(escaped, "a\\\"b\\\\n");
    }

    #[test]
    fn append_json_header_includes_meta() {
        let mut buf = String::new();
        let errors = vec!["ss failed: permission denied".to_string()];
        append_json_header(&mut buf, "all", "lsof", 1234, &errors);
        assert_eq!(
            buf,
            "{\"mode\":\"all\",\"source\":\"lsof\",\"timestamp\":1234,\"errors\":[\"ss failed: permission denied\"]"
        );
    }

    #[test]
    fn build_text_meta_lines_includes_errors() {
        let errors = vec!["fallback: ss failed".to_string(), "lsof warning".to_string()];
        let lines = build_text_meta_lines("lsof", 1700000000, &errors);

        assert_eq!(lines[0], "meta source: lsof");
        assert_eq!(lines[1], "meta timestamp: 1700000000");
        assert_eq!(lines[2], "meta errors: 2");
        assert_eq!(lines[3], "meta error: fallback: ss failed");
        assert_eq!(lines[4], "meta error: lsof warning");
    }

    #[test]
    fn aggregate_listeners_merges_endpoints() {
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
    fn append_aggregated_listener_json_includes_endpoints() {
        let listener = AggregatedListener {
            port: 443,
            pid: Some(77),
            command: "nginx".to_string(),
            user: "root".to_string(),
            endpoints: vec!["*:443".to_string(), "[::]:443".to_string()],
        };

        let mut buf = String::new();
        append_aggregated_listener_json(&mut buf, &listener);
        assert_eq!(
            buf,
            "{\"port\":443,\"pid\":77,\"command\":\"nginx\",\"user\":\"root\",\"endpoint\":\"*:443\",\"endpoints\":[\"*:443\",\"[::]:443\"],\"role\":{\"description\":\"Web server or reverse proxy\",\"confidence\":\"high\"}}"
        );
    }
}
