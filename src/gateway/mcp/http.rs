//! The MCP server over HTTP: listening, and refusing requests addressed to a
//! host name other than this computer's or sent from a web page of another
//! computer. What rmcp answers with goes out as rmcp makes it.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::task::TaskTracker;

use super::server::McpServer;
use super::{McpError, McpHttpListener, McpServerParts};

#[cfg(test)]
mod tests;

/// The path the MCP server answers on.
const PATH: &str = "/mcp";

/// Starts listening on every address that `host` names, with the same port.
pub(super) async fn listen(host: &str, port: u16) -> Result<McpHttpListener, McpError> {
    let listening = match resolve(host, port).await {
        Ok(ips) => bind_all(ips, port).await,
        Err(error) => Err(error),
    };
    listening.map_err(|source| McpError::AddressUnavailable {
        host: host.to_owned(),
        port,
        source,
    })
}

/// The addresses that `host` names. An IP address names only itself.
async fn resolve(host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    distinct(tokio::net::lookup_host((host, port)).await?)
}

/// The addresses a name resolved to, each once, in the order it resolved to
/// them. An error when there is none.
fn distinct(addrs: impl IntoIterator<Item = SocketAddr>) -> io::Result<Vec<IpAddr>> {
    let mut ips: Vec<IpAddr> = Vec::new();
    for addr in addrs {
        if !ips.contains(&addr.ip()) {
            ips.push(addr.ip());
        }
    }
    if ips.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the name resolved to no address",
        ));
    }
    Ok(ips)
}

/// Starts listening on each address with the same port. With port 0, the
/// port that the OS chose for the first address is used for the others. On
/// an error, the listening started so far ends as the listeners drop.
async fn bind_all(ips: Vec<IpAddr>, port: u16) -> io::Result<McpHttpListener> {
    let mut listeners = Vec::new();
    let mut addrs = Vec::new();
    let mut port_in_use = port;
    for ip in ips {
        let listener = TcpListener::bind(SocketAddr::new(ip, port_in_use)).await?;
        let addr = listener.local_addr()?;
        port_in_use = addr.port();
        listeners.push(listener);
        addrs.push(addr);
    }
    Ok(McpHttpListener { listeners, addrs })
}

/// Serves on every listener until the process ends. `axum::serve` keeps
/// going after a failed accept, and rmcp answers a failed exchange with one
/// client without stopping, so nothing ends a listener's server.
pub(super) async fn serve(listeners: Vec<TcpListener>, parts: McpServerParts) {
    let router = router(parts);
    let mut servers = JoinSet::new();
    for listener in listeners {
        servers.spawn(axum::serve(listener, router.clone()).into_future());
    }
    while let Some(ended) = servers.join_next().await {
        if let Err(error) = ended
            && error.is_panic()
        {
            std::panic::resume_unwind(error.into_panic());
        }
    }
}

/// The routes: rmcp's HTTP service on [`PATH`], behind the checks of the host
/// name and the origin.
fn router(parts: McpServerParts) -> Router {
    // The tool calls of all the sessions. Nothing waits for them: the server
    // runs until the process ends.
    let calls = TaskTracker::new();
    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(parts.clone(), calls.clone())),
        Arc::new(LocalSessionManager::default()),
        // The host name and the origin are checked by `refuse_other_hosts` and
        // `refuse_other_origins`, which accept every address in 127.0.0.0/8.
        // rmcp's lists of allowed hosts and origins cannot, as they match a
        // host only when it is written the same.
        StreamableHttpServerConfig::default()
            .disable_allowed_hosts()
            .disable_allowed_origins(),
    );
    Router::new()
        .route_service(PATH, service)
        .layer(middleware::from_fn(refuse_other_origins))
        .layer(middleware::from_fn(refuse_other_hosts))
}

/// Refuses a request unless the host name it is addressed to names this
/// computer, so that a web page whose own name resolves to a loopback address
/// cannot make the browser call the server.
async fn refuse_other_hosts(request: Request, next: Next) -> Response {
    if names_this_computer(request.headers(), request.uri()) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        "the host name of the request is not a loopback host name",
    )
        .into_response()
}

/// Whether the host of the request (the `Host` header, or the host in the
/// URI when there is none) is `localhost` or a loopback address, as
/// [`is_loopback_authority`] reads it.
fn names_this_computer(headers: &HeaderMap, uri: &Uri) -> bool {
    match headers.get(header::HOST) {
        Some(host) => host.to_str().is_ok_and(is_loopback_authority),
        None => uri
            .authority()
            .is_some_and(|authority| is_loopback_authority(authority.as_str())),
    }
}

/// Refuses a request whose `Origin` header names a web page of another
/// computer, so that a script of a page of another site open in the browser
/// cannot call the server. A request without the header is let through, so
/// that clients that are not browsers, which need not send it, keep working.
async fn refuse_other_origins(request: Request, next: Next) -> Response {
    if origins_name_this_computer(request.headers()) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        "the origin of the request is not a loopback origin",
    )
        .into_response()
}

/// Whether every `Origin` header of the request names a web page of this
/// computer. True when there is none.
fn origins_name_this_computer(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ORIGIN)
        .iter()
        .all(origin_names_this_computer)
}

/// Whether the value of an `Origin` header is an `http` or `https` origin
/// whose host is `localhost` or a loopback address: `http://` or `https://`
/// followed by what [`is_loopback_authority`] takes. `null`, and a list of
/// several origins, are not.
fn origin_names_this_computer(origin: &HeaderValue) -> bool {
    origin
        .to_str()
        .ok()
        .and_then(after_web_scheme)
        .is_some_and(is_loopback_authority)
}

/// What follows `http://` or `https://` (in either case) at the start of
/// `origin`. `None` when it starts with neither.
fn after_web_scheme(origin: &str) -> Option<&str> {
    ["http://", "https://"].into_iter().find_map(|scheme| {
        let (start, rest) = origin.split_at_checked(scheme.len())?;
        start.eq_ignore_ascii_case(scheme).then_some(rest)
    })
}

/// Whether `authority` is only a host and, if any, a port, and the host is
/// `localhost` or a loopback address. The port is not looked at. A user name
/// before the host, or a path, a query, or a fragment after it, is not taken.
fn is_loopback_authority(authority: &str) -> bool {
    authority
        .parse::<Authority>()
        .is_ok_and(|parsed| !parsed.as_str().contains('@') && is_loopback_host(parsed.host()))
}

/// Whether a host as written in a URI (an IPv6 address in brackets) is
/// `localhost` or a loopback address: an address in 127.0.0.0/8, `::1`, or an
/// address in 127.0.0.0/8 written in IPv6 (such as `::ffff:127.0.0.1`). The
/// name is not resolved, because what it resolves to is up to whoever owns the
/// name.
fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|ip| ip.to_canonical().is_loopback())
}
