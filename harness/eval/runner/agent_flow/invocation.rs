use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub struct Invocation {
    pub exit_code: i32,
    pub value: Option<serde_json::Value>,
    pub parse_error: Option<String>,
    pub output_bytes: usize,
}

pub fn run_cli(sinter: &Path, repository: &Path, args: &[String]) -> Result<Invocation> {
    let mut full_args = args.to_vec();
    full_args.extend([
        "--repo".to_owned(),
        repository.display().to_string(),
        "--json".to_owned(),
    ]);
    let output = Command::new(sinter)
        .args(&full_args)
        .output()
        .with_context(|| format!("failed to start sinter {}", full_args.join(" ")))?;
    let value = serde_json::from_slice(&output.stdout);
    let parse_error = value.as_ref().err().map(|error| {
        format!(
            "invalid CLI JSON: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    Ok(Invocation {
        exit_code: output.status.code().unwrap_or(-1),
        value: value.ok(),
        parse_error,
        output_bytes: output.stdout.len() + output.stderr.len(),
    })
}

pub fn run_mcp(
    sinter: &Path,
    repository: &Path,
    tool: &str,
    arguments: serde_json::Value,
) -> Result<Invocation> {
    let mut child = Command::new(sinter)
        .args(["serve", "--repo"])
        .arg(repository)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start sinter serve")?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    });
    writeln!(
        child.stdin.as_mut().context("sinter serve has no stdin")?,
        "{request}"
    )?;
    drop(child.stdin.take());
    let output = child
        .wait_with_output()
        .context("failed to wait for sinter serve")?;
    let response = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose();
    let (exit_code, value, protocol_error) = match response {
        Ok(Some(response)) if response.get("error").is_some() => (
            1,
            response
                .pointer("/error/data")
                .or_else(|| response.get("error"))
                .cloned(),
            None,
        ),
        Ok(Some(response)) => match mcp_body(&response) {
            Ok(value) => (0, Some(value), None),
            Err(error) => (0, None, Some(format!("{error:#}"))),
        },
        Ok(None) => (0, None, Some("MCP server returned no response".to_owned())),
        Err(error) => (0, None, Some(format!("invalid MCP response JSON: {error}"))),
    };
    Ok(Invocation {
        exit_code,
        value,
        parse_error: protocol_error,
        output_bytes: output.stdout.len() + output.stderr.len(),
    })
}

fn mcp_body(response: &serde_json::Value) -> Result<serde_json::Value> {
    if let Some(value) = response.pointer("/result/structuredContent") {
        return Ok(value.clone());
    }
    let text = response
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .context("MCP response has neither structuredContent nor JSON text")?;
    serde_json::from_str(text).context("MCP text content is not JSON")
}
