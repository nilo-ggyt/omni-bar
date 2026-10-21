//! Observe one external HTTP handoff with one owned listener, worker and terminal receipt.
//! This observer neither controls the browser nor owns any document or browser-history entry.

use super::{StepTerminalSignal, hosted_step_terminal};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Preserve actual server facts; late observers consume the same result instead of trusting openURL.
struct Receipt {
    armed: bool,
    connections: u8,
    empty_connections: u8,
    nonce_get_received: bool,
    response_written: bool,
    result: Option<Result<(), String>>,
    signal: Option<StepTerminalSignal>,
}

/// Own the synthetic URL, cancellation endpoint and join obligation for this handoff alone.
pub(super) struct ExternalHttp {
    url: String,
    receipt: Arc<Mutex<Receipt>>,
    cancel: Option<UnixStream>,
    worker: Option<JoinHandle<()>>,
}

impl ExternalHttp {
    /// Bind a kernel-assigned loopback port and nonce without changing the default browser or schemes.
    pub(super) fn new() -> Result<Self, String> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| "无法建立本轮HTTP监听器")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "无法配置本轮HTTP监听器")?;
        let address = listener.local_addr().map_err(|_| "无法取得本轮HTTP端口")?;
        // The fresh path prevents another run's request from satisfying this listener's receipt.
        let mut random = [0_u8; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "无法取得本轮HTTP随机身份")?;
        let nonce = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = format!("/omni-accept/{nonce}");
        let url = format!("http://{address}{path}");
        // A private socket wakes poll during cleanup even if the browser never connects.
        let (cancel, cancellation) = UnixStream::pair().map_err(|_| "无法建立HTTP取消通道")?;
        let receipt = Arc::new(Mutex::new(Receipt {
            armed: false,
            connections: 0,
            empty_connections: 0,
            nonce_get_received: false,
            response_written: false,
            result: None,
            signal: None,
        }));
        let state = receipt.clone();
        let worker = std::thread::Builder::new()
            .name("omni-http-handoff".into())
            .spawn(move || {
                let result = serve(
                    &listener,
                    &cancellation,
                    &address.to_string(),
                    &path,
                    &state,
                );
                // Return both owned descriptors before notification; queued completion alone proves no release.
                drop(listener);
                drop(cancellation);
                // This terminal describes only the server; writing a response proves no browser-tab cleanup.
                eprintln!(
                    "external_http event=server_terminal outcome={}",
                    if result.is_ok() { "success" } else { "failed" }
                );
                let signal = match state.lock() {
                    Ok(mut state) => {
                        state.result = Some(result);
                        state.signal.take()
                    }
                    Err(_) => None, // Verification reports poison; an unknown result never becomes success.
                };
                if let Some(signal) = signal {
                    notify(signal);
                }
            })
            .map_err(|_| "无法启动本轮HTTP服务线程")?;
        // Record only this generated loopback identity for precise cleanup, never a user URL or document.
        eprintln!("external_http event=listener_created url={url}");
        Ok(Self {
            url,
            receipt,
            cancel: Some(cancel),
            worker: Some(worker),
        })
    }

    /// Lend the exact nonce URL while its listener owner remains available to the driver.
    pub(super) fn url(&self) -> &str {
        &self.url
    }

    /// Arm before the actual mouse action; unsolicited prefetch and stale requests cannot authorize receipt.
    pub(super) fn arm(&self) -> Result<(), String> {
        let mut state = self.receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
        if state.armed || state.result.is_some() {
            return Err("本轮HTTP交接重复登记或已结束".into());
        }
        state.armed = true;
        Ok(())
    }

    /// Observe after Host binds the original step signal; completion before registration uses the same receipt.
    pub(super) fn observe(&self, signal: StepTerminalSignal) -> Result<bool, String> {
        let mut state = self.receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
        if !state.armed || state.signal.is_some() {
            return Err("HTTP终点观察没有原点击权限或重复登记".into());
        }
        if state.result.is_some() {
            return Ok(true);
        }
        state.signal = Some(signal);
        Ok(false)
    }

    /// Prove only the original GET and complete 204 write, never rendering, tab disposal or history cleanup.
    pub(super) fn verify(&self) -> Result<(), String> {
        let state = self.receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
        match &state.result {
            Some(Ok(())) if state.armed && state.nonce_get_received && state.response_written => {
                Ok(())
            }
            Some(Err(error)) => Err(error.clone()),
            _ => Err("本轮HTTP尚无真实GET及完整204响应写入".into()),
        }
    }

    /// Inspect the network owner's real progress at the Host deadline, after relative navigation released Body.
    pub(super) fn diagnose_deadline(&self) {
        let snapshot = self.receipt.lock().map(|state| {
            (
                state.armed,
                state.connections,
                state.empty_connections,
                state.nonce_get_received,
                state.response_written,
                state.result.clone(),
                state.signal.is_some(),
            )
        }); // Copy bounded counters and fixed errors; log outside the mutex without inspecting page contents.
        match snapshot {
            Ok((armed, connections, empty, get, written, result, signal)) => eprintln!(
                "external_http event=deadline armed={armed} connections={connections} empty_connections={empty} actual_nonce_get={get} response_write_complete={written} terminal={} signal_registered={signal} error={:?}",
                match &result {
                    None => "pending",
                    Some(Ok(())) => "success",
                    Some(Err(_)) => "failed",
                },
                result.and_then(Result::err),
            ),
            Err(_) => eprintln!("external_http event=deadline receipt_poisoned=true"),
        }
    }

    /// Wake poll by dropping the private socket; bounded connection deadlines also bound error cleanup.
    pub(super) fn shutdown(&mut self) -> Result<(), String> {
        drop(self.cancel.take());
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "本轮HTTP服务线程异常退出")?;
        }
        Ok(())
    }
}

impl Drop for ExternalHttp {
    /// Retain the join obligation on early return; ordinary driver cleanup reports shutdown errors explicitly.
    fn drop(&mut self) {
        // An untransferred constructor result still releases its worker and port during unwind.
        let _ = self.shutdown();
    }
}

/// Return only the existing scalar authority to Host's original main-queue completion path.
fn notify(signal: StepTerminalSignal) {
    // The worker never borrows Native, Driver or text; the main queue revalidates the original step.
    dispatch2::DispatchQueue::main().exec_async(move || {
        eprintln!(
            "external_http event=terminal_signal_dispatched step_index={}",
            signal.index,
        );
        hosted_step_terminal(signal);
    });
}

/// Wait on the original listener and cancellation socket without timer polling or browser-action retries.
fn serve(
    listener: &TcpListener,
    cancellation: &UnixStream,
    host: &str,
    path: &str,
    receipt: &Mutex<Receipt>,
) -> Result<(), String> {
    for _ in 0..16 {
        // Cancellation HUP terminates the worker even when no browser connection ever reaches the listener.
        let mut waits = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: cancellation.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: Both borrowed descriptors stay live here; nfds exactly matches the allocated array.
            if unsafe { libc::poll(waits.as_mut_ptr(), waits.len() as libc::nfds_t, -1) } >= 0 {
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err("本轮HTTP资源等待失败".into());
            }
        }
        if waits[1].revents != 0 {
            // Cancellation takes precedence over a simultaneous ready connection; do not accept fresh work.
            return Err("本轮HTTP交接已取消".into());
        }
        if waits[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err("本轮HTTP监听器已失效".into());
        }
        let (mut stream, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => return Err("无法接收本轮HTTP连接".into()),
        };
        if !peer.ip().is_loopback() {
            return Err("本轮HTTP收到非本机连接".into());
        }
        {
            // The outer sixteen-connection budget bounds counts while distinguishing idle preconnect from GET.
            let mut state = receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
            state.connections += 1;
        }
        stream
            .set_nonblocking(false)
            .map_err(|_| "无法设置HTTP连接阻塞期限")?;
        let started = Instant::now();
        let Some(request) = read_request(&mut stream, started)? else {
            // Browsers may preconnect without sending HTTP. Close only this idle connection, without replaying navigation.
            // It still consumes the original connection budget and cannot extend Host's three-second deadline.
            receipt
                .lock()
                .map_err(|_| "HTTP交接事实已poison")?
                .empty_connections += 1;
            continue;
        };
        if request.host != host {
            // A request to another authority cannot prove delivery of this run's exact URL.
            return Err("本轮HTTP的目标身份不符".into());
        }
        if request.method == "GET" && request.path == path && request.body.is_empty() {
            {
                let mut state = receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
                if !state.armed || state.response_written {
                    // Only the one armed mouse action may authorize one complete response.
                    return Err("HTTP交付请求未登记或已结束".into());
                }
                state.nonce_get_received = true;
            }
            eprintln!("external_http event=nonce_get_received");
            // A bodyless 204 adds no HTML or script; it still cannot prove the browser disposed of a tab or history.
            // Keep the connection's original one-second deadline; a partial write never produces a complete receipt.
            respond_no_content(&mut stream, started)?;
            receipt
                .lock()
                .map_err(|_| "HTTP交接事实已poison")?
                .response_written = true;
            eprintln!("external_http event=response_written status=204 browser_cleanup=unverified");
            return Ok(()); // Drop the stream, then release the listener before signaling; tab lifetime stays unverified.
        } else {
            return Err("本轮HTTP收到未声明的请求".into());
        }
    }
    Err("本轮HTTP连接数量超过有限交接范围".into())
}

/// Hold only the bounded HTTP envelope needed to match this synthetic handoff.
struct Request {
    method: String,
    path: String,
    host: String,
    body: Vec<u8>,
}

/// Admit at most 8 KiB of HTTP/1.1 headers and 128 payload bytes; reject chunking and duplicate lengths.
fn read_request(stream: &mut TcpStream, started: Instant) -> Result<Option<Request>, String> {
    let mut bytes = Vec::with_capacity(1024);
    let header_end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() >= 8192 {
            // Bound pre-terminator growth before another fixed-size read; the exact header end is checked below.
            return Err("本轮HTTP请求头超过边界".into());
        }
        stream
            .set_read_timeout(Some(remaining(started)?))
            .map_err(|_| "无法限制HTTP读取期限")?;
        let mut page = [0_u8; 512];
        let count = match stream.read(&mut page) {
            Ok(count) => count,
            Err(error)
                if bytes.is_empty()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
            {
                // Zero received bytes identify idle preconnect; any partially received request must report timeout.
                return Ok(None);
            }
            Err(_) => return Err("本轮HTTP请求读取失败或超时".into()),
        };
        if count == 0 {
            if bytes.is_empty() {
                return Ok(None); // An unused preconnection may close, but never counts as HTTP delivery.
            }
            return Err("本轮HTTP请求在头部结束前断开".into());
        }
        bytes.extend_from_slice(&page[..count]);
    };
    if header_end > 8192 {
        return Err("本轮HTTP请求头超过边界".into());
    }
    // Decode only the bounded header and require one unambiguous version, authority and payload length.
    let text = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "本轮HTTP头编码无效")?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().ok_or("本轮HTTP缺少请求行")?.split_whitespace();
    let method = first.next().ok_or("本轮HTTP缺少方法")?.to_owned();
    let path = first.next().ok_or("本轮HTTP缺少路径")?.to_owned();
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return Err("本轮HTTP版本不符".into());
    }
    let (mut host, mut length) = (None, None);
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("本轮HTTP头格式无效")?;
        if name.eq_ignore_ascii_case("host") {
            if host.replace(value.trim().to_owned()).is_some() {
                return Err("本轮HTTP重复目标头".into());
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            let value = value
                .trim()
                .parse::<usize>()
                .map_err(|_| "本轮HTTP回执长度无效")?;
            if value > 128 || length.replace(value).is_some() {
                return Err("本轮HTTP回执超限或重复长度".into());
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("本轮HTTP不接收分块载荷".into());
        }
    }
    let host = host.ok_or("本轮HTTP没有目标头")?;
    let length = length.unwrap_or(0);
    let mut body = bytes[header_end..].to_vec();
    if body.len() > length {
        // Pipelined or undeclared bytes cannot become part of this one-request receipt.
        return Err("本轮HTTP出现声明外的尾部".into());
    }
    while body.len() < length {
        // Read only the declared remainder and debit the same absolute connection budget on each read.
        stream
            .set_read_timeout(Some(remaining(started)?))
            .map_err(|_| "无法限制HTTP回执期限")?;
        let mut page = [0_u8; 128];
        let maximum = length - body.len();
        let count = stream
            .read(&mut page[..maximum])
            .map_err(|_| "本轮HTTP回执读取失败或超时")?;
        if count == 0 {
            return Err("本轮HTTP回执未完整交付".into());
        }
        body.extend_from_slice(&page[..count]);
    }
    Ok(Some(Request {
        method,
        path,
        host,
        body,
    }))
}

/// Debit all reads and writes from one start time instead of renewing a timeout after partial progress.
fn remaining(started: Instant) -> Result<Duration, String> {
    Duration::from_secs(1)
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| "本轮HTTP连接超过一秒期限".into())
}

/// Write only 204 headers; the stream and worker owners remain responsible for their actual release.
fn respond_no_content(stream: &mut TcpStream, started: Instant) -> Result<(), String> {
    // Produce an actual Date with stack-owned UTC storage, without global locale changes or a date cache.
    // SAFETY: time accepts a null output pointer; the initialized return value is used only after success.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut utc = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: gmtime_r synchronously borrows allocated stack storage; only a nonnull return permits reading it.
    if now == -1 || unsafe { libc::gmtime_r(&now, utc.as_mut_ptr()) }.is_null() {
        return Err("无法取得本轮HTTP响应UTC时间".into());
    }
    // SAFETY: The successful call initialized tm completely; no pointer escapes into response writing.
    let utc = unsafe { utc.assume_init() };
    // Format the protocol's fixed English weekday/month tokens without consulting process locale.
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .get(utc.tm_wday as usize)
        .ok_or("本轮HTTP响应星期无效")?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .get(utc.tm_mon as usize)
    .ok_or("本轮HTTP响应月份无效")?;
    let year = utc
        .tm_year
        .checked_add(1900)
        .filter(|year| (0..=9999).contains(year))
        .ok_or("本轮HTTP响应年份无效")?;
    let bytes = format!(
        "HTTP/1.1 204 No Content\r\nDate: {weekday}, {:02} {month} {year:04} {:02}:{:02}:{:02} GMT\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        utc.tm_mday, utc.tm_hour, utc.tm_min, utc.tm_sec,
    );
    let mut pending = bytes.as_bytes();
    while !pending.is_empty() {
        // Date generation and partial writes all consume the original second; none renews the budget.
        stream
            .set_write_timeout(Some(remaining(started)?))
            .map_err(|_| "无法限制HTTP响应期限")?;
        let count = stream
            .write(pending)
            .map_err(|_| "本轮HTTP响应失败或超时")?;
        if count == 0 {
            return Err("本轮HTTP响应在完成前断开".into());
        }
        pending = &pending[count..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ExternalHttp;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    /// Exercise the real server's externally visible bytes and returned port through its public owner API.
    #[test]
    fn armed_get_completes_with_bodyless_204_and_releases_listener() {
        let mut server = ExternalHttp::new().expect("建立真实本机监听器");
        server.arm().expect("登记本次交付");
        let target = server.url().strip_prefix("http://").expect("本机HTTP URL");
        let (address, path) = target.split_once('/').expect("完整nonce路径");
        let address = address.to_owned();
        let request =
            format!("GET /{path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
        let mut client = TcpStream::connect(&address).expect("连接原监听器");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("限制真实读取");
        client
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("限制真实写入");

        // Send a real request to the production URL without replacing serve, registering AppKit or mutating receipt.
        client.write_all(request.as_bytes()).expect("实际写入原GET");
        let mut response = Vec::new();
        let read = client.read_to_end(&mut response);
        let joined = server.shutdown();
        drop(client); // End both sides before assertions; the owner's Drop still joins on failure or panic.
        read.expect("服务端写完并实际关闭连接");
        joined.expect("原worker实际返回并join");
        server.verify().expect("原真实GET及完整响应已完成");

        // Judge actual network bytes: a 204 ends at the header boundary without HTML, scripts or body framing.
        let response = std::str::from_utf8(&response).expect("响应头UTF-8");
        let (headers, body) = response.split_once("\r\n\r\n").expect("完整响应头尾");
        assert_eq!(headers.lines().next(), Some("HTTP/1.1 204 No Content"));
        assert!(body.is_empty(), "204不能带正文");
        assert!(headers.lines().any(|line| line.starts_with("Date: ")));
        assert!(!headers.lines().any(|line| {
            let name = line.split_once(':').map(|(name, _)| name).unwrap_or("");
            name.eq_ignore_ascii_case("Content-Length")
                || name.eq_ignore_ascii_case("Transfer-Encoding")
        }));
        assert!(TcpStream::connect(&address).is_err(), "原监听端口已结束");
    }

    /// An unsolicited real connection must receive no successful response and must release the listener.
    #[test]
    fn unarmed_get_is_rejected_without_a_success_response() {
        let mut server = ExternalHttp::new().expect("建立真实本机监听器");
        let target = server.url().strip_prefix("http://").expect("本机HTTP URL");
        let (address, path) = target.split_once('/').expect("完整nonce路径");
        let request =
            format!("GET /{path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
        let mut client = TcpStream::connect(address).expect("连接原监听器");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("限制真实读取");
        client
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("限制真实写入");

        // Valid wire bytes cannot replace admission by the original action's arm operation.
        client
            .write_all(request.as_bytes())
            .expect("实际写入未登记GET");
        let mut response = Vec::new();
        let read = client.read_to_end(&mut response);
        let joined = server.shutdown();
        drop(client);
        read.expect("未登记请求实际关闭连接");
        joined.expect("失败worker实际join");
        assert!(response.is_empty(), "未登记请求不得收到成功响应");
        assert!(server.verify().is_err(), "拒绝请求不得伪报交付成功");
    }

    /// Cancel an idle real listener through its owned Unix socket without manufacturing a browser request.
    #[test]
    fn armed_listener_without_a_request_can_be_cancelled_and_joined() {
        let mut server = ExternalHttp::new().expect("建立真实本机监听器");
        server.arm().expect("登记本次交付");
        let target = server.url().strip_prefix("http://").expect("本机HTTP URL");
        let (address, _) = target.split_once('/').expect("完整nonce路径");
        let address = address.to_owned();

        // No TCP connection or extra request is sent; production cancellation must wake poll and join the worker.
        let started = Instant::now();
        server.shutdown().expect("空闲监听器取消后实际join");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(server.verify().is_err(), "取消不能当作HTTP交付成功");
        assert!(
            TcpStream::connect(&address).is_err(),
            "取消后原监听端口已结束"
        );
    }
}
