//! 手机推送接收端：iPhone 的「快捷指令」把短信、应用通知发到本机，泡浮在岛上显示。
//!
//! - 只监听局域网（`0.0.0.0:端口`），每个请求都要带配对令牌，令牌不对一律 401。
//! - `POST /push?token=…` 收 JSON：`{"app":"微信","title":"张三","text":"在吗"}`；
//!   也接受 GET 查询参数，方便在电脑浏览器里先试一次。
//! - `GET /ping` 只回 `{"ok":true}`，用来确认手机能不能连上电脑。
//! - 网络线程只做解析，收到的消息通过 `mpsc` 交给界面线程，不直接碰 egui。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_PORT: u32 = 8790;
/// 岛上显示用的正文上限，超长内容截断，避免气泡被撑开。
const TEXT_LIMIT: usize = 160;

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub app: String,
    pub title: String,
    pub text: String,
    /// 收到时间，`HH:MM`。
    pub at: String,
}

/// 交给设置页显示的一份快照。
#[derive(Clone, Debug)]
pub struct Runtime {
    pub status: String,
    pub token: String,
    pub url: String,
    pub port: u32,
    pub running: bool,
}

pub struct Service {
    rx: Receiver<Message>,
    stop: Arc<AtomicBool>,
    runtime: Runtime,
}

impl Service {
    pub fn start(port: u32, token: String, ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let token = token.trim().to_string();
        let url = format!(
            "http://{}:{}/push?token={}",
            lan_ip().unwrap_or_else(|| "电脑局域网IP".into()),
            port,
            token
        );
        let (runtime, listener) = match TcpListener::bind(("0.0.0.0", port as u16)) {
            Ok(listener) => (
                Runtime {
                    status: format!("正在监听 {port} 端口，等待手机推送"),
                    token: token.clone(),
                    url: url.clone(),
                    port,
                    running: true,
                },
                Some(listener),
            ),
            Err(e) => (
                Runtime { status: bind_hint(port, &e), token: token.clone(), url: url.clone(), port, running: false },
                None,
            ),
        };
        if let Some(listener) = listener {
            listener.set_nonblocking(true).ok();
            spawn(listener, token, tx, stop.clone(), ctx);
        }
        Self { rx, stop, runtime }
    }

    pub fn runtime(&self) -> Runtime {
        self.runtime.clone()
    }

    /// 把攒下的消息一次性取走；界面每帧调用，没有消息时是空表。
    pub fn drain(&self) -> Vec<Message> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn spawn(listener: TcpListener, token: String, tx: Sender<Message>, stop: Arc<AtomicBool>, ctx: eframe::egui::Context) {
    std::thread::Builder::new()
        .name("phone-bridge".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Some(message) = serve(stream, &token) {
                            if tx.send(message).is_ok() {
                                ctx.request_repaint();
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(60))
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}

fn serve(mut stream: TcpStream, token: &str) -> Option<Message> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1200)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(1200)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut continued = false;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..end]);
                    // 有些客户端（PowerShell 的 Invoke-RestMethod 等）会先等 100 Continue 再发正文。
                    if !continued && header(&head, "expect").is_some_and(|v| v.to_ascii_lowercase().contains("100-continue")) {
                        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
                        let _ = stream.flush();
                        continued = true;
                    }
                    let need = header(&head, "content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                    if buf.len() >= end + 4 + need {
                        break;
                    }
                }
                if buf.len() > 16 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let end = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let (head, body) = match end {
        Some(i) => (String::from_utf8_lossy(&buf[..i]).into_owned(), &buf[i + 4..]),
        None => (String::from_utf8_lossy(&buf).into_owned(), &buf[..0]),
    };
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut params: HashMap<String, String> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(kv), String::new()),
        })
        .collect();
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(body) {
        for (key, value) in map {
            let text = match value {
                serde_json::Value::String(s) => s,
                serde_json::Value::Null => continue,
                other => other.to_string(),
            };
            params.entry(key).or_insert(text);
        }
    }

    let given = params
        .get("token")
        .cloned()
        .or_else(|| headers.get("x-token").cloned())
        .unwrap_or_default();
    if token.is_empty() || given.trim() != token {
        reply(&mut stream, "401 Unauthorized", "{\"ok\":false,\"error\":\"token\"}");
        return None;
    }
    if path == "/ping" {
        reply(&mut stream, "200 OK", "{\"ok\":true,\"app\":\"泡浮\"}");
        return None;
    }
    if path != "/push" {
        reply(&mut stream, "404 Not Found", "{\"ok\":false,\"error\":\"path\"}");
        return None;
    }
    if method != "POST" && method != "GET" {
        reply(&mut stream, "405 Method Not Allowed", "{\"ok\":false,\"error\":\"method\"}");
        return None;
    }

    let app = field(&params, &["app", "from", "source", "name"])
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "iPhone".into());
    let title = field(&params, &["title", "subject", "sender"]).unwrap_or_default();
    let text = field(&params, &["text", "body", "message", "content", "msg"]).unwrap_or_default();
    if title.trim().is_empty() && text.trim().is_empty() {
        reply(&mut stream, "400 Bad Request", "{\"ok\":false,\"error\":\"empty\"}");
        return None;
    }
    reply(&mut stream, "200 OK", "{\"ok\":true}");
    Some(Message {
        app: clip(app.trim(), 24),
        title: clip(title.trim(), 40),
        text: clip(text.trim(), TEXT_LIMIT),
        at: chrono::Local::now().format("%H:%M").to_string(),
    })
}

fn field(params: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| params.get(*key).map(|v| v.trim().to_string()))
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn reply(stream: &mut TcpStream, code: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {code}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
        body.as_bytes().len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn header(head: &str, name: &str) -> Option<String> {
    head.split("\r\n").find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim().eq_ignore_ascii_case(name)).then(|| value.trim().to_string())
    })
}

fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn bind_hint(port: u32, err: &std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        format!("端口 {port} 已被其他程序占用，换一个端口再试")
    } else {
        format!("无法监听 {port} 端口：{err}")
    }
}

/// 六位配对令牌，用来挡住同网段的其他设备乱发消息。
pub fn new_token() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seed = (nanos / 7) as u64 ^ (std::process::id() as u64) * 7919;
    format!("{:06}", (seed % 900000) + 100000)
}

/// 取本机在局域网里的 IPv4：连一下 UDP 只让系统选出口网卡，不会真的发包。
fn lan_ip() -> Option<String> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("223.5.5.5", 53)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback()).then(|| ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(decode("a%20b+c"), "a b c");
        assert_eq!(decode("%E4%BD%A0%E5%A5%BD"), "你好");
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn clips_long_text() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 3), "ab…");
    }

    #[test]
    fn token_looks_six_digits() {
        let token = new_token();
        assert_eq!(token.len(), 6);
        assert!(token.parse::<u32>().is_ok());
    }
}
