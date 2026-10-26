//! Blocking client side of the local transport, for the short-lived
//! commands (`tower hook`, `tower mcp`, `tower status`): a Unix socket, or a
//! named pipe (`\\.\pipe\...`) on Windows.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;
use tower_proto::{decode_line, encode_line, ClientMsg, ServerMsg, Snapshot, PROTOCOL_VERSION};

pub type Reader = BufReader<Box<dyn Read + Send>>;
pub type Writer = Box<dyn Write + Send>;

#[cfg(unix)]
fn open(sock: &Path, timeout: Duration) -> std::io::Result<(Box<dyn Read + Send>, Writer)> {
    let s = std::os::unix::net::UnixStream::connect(sock)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    Ok((Box::new(s.try_clone()?), Box::new(s)))
}

/// A pipe client is a file opened read/write; Windows has no timeouts on it,
/// which is acceptable for hooks (Claude kills slow hooks) and the CLI.
#[cfg(windows)]
fn open(sock: &Path, _timeout: Duration) -> std::io::Result<(Box<dyn Read + Send>, Writer)> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(sock)?;
    Ok((Box::new(f.try_clone()?), Box::new(f)))
}

/// Connect and say hello; returns the line reader and the writer.
pub fn hello(
    sock: &Path,
    token: &str,
    client: &str,
    timeout: Duration,
) -> anyhow::Result<(Reader, Writer)> {
    let (r, mut w) = open(sock, timeout)?;
    w.write_all(
        encode_line(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            token: token.trim().to_string(),
            client: client.into(),
        })?
        .as_bytes(),
    )?;
    let mut r = BufReader::new(r);
    match read_msg(&mut r)? {
        ServerMsg::Hello { .. } => Ok((r, w)),
        ServerMsg::Error { message } => anyhow::bail!("towerd: {message}"),
        other => anyhow::bail!("unexpected {other:?}"),
    }
}

pub fn read_msg(r: &mut Reader) -> anyhow::Result<ServerMsg> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        anyhow::bail!("towerd closed the connection");
    }
    Ok(decode_line::<ServerMsg>(&line)?)
}

/// A "cli" client's request: hello, send `msg`, and return its reply. The
/// daemon pushes a Snapshot to a cli client right after hello and on every
/// change, so those are skipped until the actual reply arrives.
pub fn call(
    sock: &Path,
    token: &str,
    msg: &ClientMsg,
    timeout: Duration,
) -> anyhow::Result<ServerMsg> {
    let (mut r, mut w) = hello(sock, token, "cli", timeout)?;
    w.write_all(encode_line(msg)?.as_bytes())?;
    loop {
        match read_msg(&mut r)? {
            ServerMsg::Snapshot { .. } => continue,
            other => return Ok(other),
        }
    }
}

/// The daemon's current snapshot, as a "cli" client sees it. The push that
/// follows hello is the last *published* one and can predate a change made
/// a moment ago (publishing happens on the engine's tick), so this asks for
/// a fresh one and returns that.
pub fn snapshot(sock: &Path, token: &str, timeout: Duration) -> anyhow::Result<Snapshot> {
    let (mut r, mut w) = hello(sock, token, "cli", timeout)?;
    // The welcome push.
    loop {
        match read_msg(&mut r)? {
            ServerMsg::Snapshot { .. } => break,
            ServerMsg::Error { message } => anyhow::bail!("towerd: {message}"),
            _ => continue,
        }
    }
    w.write_all(encode_line(&ClientMsg::GetSnapshot)?.as_bytes())?;
    loop {
        match read_msg(&mut r)? {
            ServerMsg::Snapshot { snapshot } => return Ok(snapshot),
            ServerMsg::Error { message } => anyhow::bail!("towerd: {message}"),
            _ => continue,
        }
    }
}

/// Hello, one request, one reply.
pub fn round_trip(
    sock: &Path,
    token: &str,
    client: &str,
    msg: &ClientMsg,
    timeout: Duration,
) -> anyhow::Result<ServerMsg> {
    let (mut r, mut w) = hello(sock, token, client, timeout)?;
    w.write_all(encode_line(msg)?.as_bytes())?;
    read_msg(&mut r)
}
