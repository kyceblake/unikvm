use std::fmt::Write;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use ring::rand::{SecureRandom, SystemRandom};
use tiny_http::{Header, Method, Response, Server};
use tokio::runtime::Builder;
use tokio::time::timeout;
use unikvm_platform::local::{Endpoint, STATUS_TIMEOUT};
use unikvm_protocol::local::{ClipboardPhase, ClipboardStatus, query_clipboard};

pub fn open(launch_browser: bool) -> Result<()> {
    let endpoint = Endpoint::for_current_user()?;
    let runtime = Builder::new_current_thread().enable_all().build()?;
    let path = private_path()?;
    let state_path = format!("{path}state");
    let server = Server::http(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| anyhow!("{error}"))
        .context("bind clipboard monitor")?;
    let address = server
        .server_addr()
        .to_ip()
        .context("get monitor address")?;
    let host = address.to_string();
    let url = format!("http://{host}{path}");
    println!("Clipboard monitor at {url}");
    println!("Press Ctrl+C to stop the monitor.");
    if launch_browser && let Err(error) = open_browser(&url) {
        eprintln!("Could not open the browser. Open the monitor URL manually. {error}");
    }

    for request in server.incoming_requests() {
        let host_matches = request
            .headers()
            .iter()
            .any(|header| header.field.equiv("Host") && header.value.as_str() == host);
        let (code, content_type, body) = match (host_matches, request.method(), request.url()) {
            (true, &Method::Get, url) if url == path => (
                200,
                "text/html; charset=utf-8",
                include_str!("clipboard.html").to_owned(),
            ),
            (true, &Method::Get, url) if url == state_path => (
                200,
                "application/json",
                state_json(runtime.block_on(read_state(&endpoint)).ok()),
            ),
            _ => (404, "text/plain; charset=utf-8", String::from("Not found")),
        };
        let mut response = Response::from_string(body).with_status_code(code);
        for (key, value) in [
            ("Content-Type", content_type),
            ("Cache-Control", "no-store"),
            ("Referrer-Policy", "no-referrer"),
            ("X-Content-Type-Options", "nosniff"),
            (
                "Content-Security-Policy",
                "default-src 'none'; connect-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'",
            ),
        ] {
            response.add_header(Header::from_bytes(key, value).expect("valid HTTP header"));
        }
        // A browser can close a tab while a poll is still in flight.
        let _ = request.respond(response);
    }
    Ok(())
}

fn private_path() -> Result<String> {
    let mut token = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut token)
        .map_err(|_| anyhow!("generate clipboard monitor token"))?;
    let mut path = String::from("/");
    for byte in token {
        write!(path, "{byte:02x}").expect("write to String");
    }
    path.push('/');
    Ok(path)
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command.arg(url).spawn().map(|_| ())
}

async fn read_state(endpoint: &Endpoint) -> Result<ClipboardStatus> {
    timeout(STATUS_TIMEOUT, async {
        let mut stream = endpoint.connect().await?;
        query_clipboard(&mut stream).await
    })
    .await?
    .context("read clipboard status")
}

fn state_json(status: Option<ClipboardStatus>) -> String {
    let online = status.is_some();
    let state = status.unwrap_or_default();
    format!(
        r#"{{"agent":{online},"connected":{},"outgoing":"{}","incoming":"{}"}}"#,
        state.connected,
        phase_name(state.outgoing),
        phase_name(state.incoming),
    )
}

fn phase_name(phase: ClipboardPhase) -> &'static str {
    match phase {
        ClipboardPhase::Idle => "idle",
        ClipboardPhase::Copying => "copying",
        ClipboardPhase::Ready => "ready",
        ClipboardPhase::Failed => "failed",
        ClipboardPhase::Skipped => "skipped",
    }
}
