//! 本轮HTTP页面交接只有一个监听器、一个工作线程和一个终点；不控制浏览器或持有任何文稿。

use super::{StepTerminalSignal, hosted_step_terminal};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RECEIPT: &[u8] = b"standard-self-close|trusted=true|persisted=false";

/// 这里保存服务器实际收讫事实；迟到观察读取同一终态，不靠点击排队或openURL返回值通过。
struct Receipt {
    armed: bool,
    connections: u8,
    empty_connections: u8,
    nonce_get_received: bool,
    get_received: bool,
    pagehide_received: bool,
    result: Option<Result<(), String>>,
    signal: Option<StepTerminalSignal>,
}

pub(super) struct ExternalHttp {
    url: String,
    receipt: Arc<Mutex<Receipt>>,
    cancel: Option<UnixStream>,
    worker: Option<JoinHandle<()>>,
}

impl ExternalHttp {
    /// 实际端口由内核分配，随机nonce只属于本轮页面；不注册协议或修改默认浏览器。
    pub(super) fn new() -> Result<Self, String> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| "无法建立本轮HTTP监听器")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "无法配置本轮HTTP监听器")?;
        let address = listener.local_addr().map_err(|_| "无法取得本轮HTTP端口")?;
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
        let (cancel, cancellation) = UnixStream::pair().map_err(|_| "无法建立HTTP取消通道")?;
        let receipt = Arc::new(Mutex::new(Receipt {
            armed: false,
            connections: 0,
            empty_connections: 0,
            nonce_get_received: false,
            get_received: false,
            pagehide_received: false,
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
                    &nonce,
                    &state,
                );
                // 终点先归还端口和取消读端；通知不是工作线程仍占监听器的替代完成标志。
                drop(listener);
                drop(cancellation);
                // 这里只报告本服务的有界终态；不把页面请求或取消伪报成浏览器卸载完成。
                eprintln!(
                    "external_http event=server_terminal outcome={}",
                    if result.is_ok() { "success" } else { "failed" }
                );
                let signal = match state.lock() {
                    Ok(mut state) => {
                        state.result = Some(result);
                        state.signal.take()
                    }
                    Err(_) => None, // 唯一校验边界会报告poison，不能把未知结果签作成功。
                };
                if let Some(signal) = signal {
                    notify(signal);
                }
            })
            .map_err(|_| "无法启动本轮HTTP服务线程")?;
        Ok(Self {
            url,
            receipt,
            cancel: Some(cancel),
            worker: Some(worker),
        })
    }

    pub(super) fn url(&self) -> &str {
        &self.url
    }

    /// 必须在原鼠标动作之前登记；服务器不接受未授权的预取或旧页面回执。
    pub(super) fn arm(&self) -> Result<(), String> {
        let mut state = self.receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
        if state.armed || state.result.is_some() {
            return Err("本轮HTTP交接重复登记或已结束".into());
        }
        state.armed = true;
        Ok(())
    }

    /// Host已经绑定原StepTerminalSignal后再观察；终点先到也消费同一实际收据一次。
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

    /// 只读取服务器的真实GET和卸载回执；不从系统打开BOOL、页面标题或浏览器进程存在推断。
    pub(super) fn verify(&self) -> Result<(), String> {
        let state = self.receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
        match &state.result {
            Some(Ok(())) if state.armed && state.get_received => Ok(()),
            Some(Err(error)) => Err(error.clone()),
            _ => Err("本轮HTTP尚无真实GET及非持久化页面卸载回执".into()),
        }
    }

    /// 原Host期限只读本网络拥有者的实际进度；相对导航前的Body已释放，不能诊断它冒充HTTP等待。
    pub(super) fn diagnose_deadline(&self) {
        let snapshot = self.receipt.lock().map(|state| {
            (
                state.armed,
                state.connections,
                state.empty_connections,
                state.nonce_get_received,
                state.get_received,
                state.pagehide_received,
                state.result.clone(),
                state.signal.is_some(),
            )
        }); // 日志I/O在mutex释放后执行，只复制有限计数与本服务的固定错误，不读取页面正文。
        match snapshot {
            Ok((armed, connections, empty, get, served, hidden, result, signal)) => eprintln!(
                "external_http event=deadline armed={armed} connections={connections} empty_connections={empty} actual_nonce_get={get} page_served={served} trusted_pagehide={hidden} terminal={} signal_registered={signal} error={:?}",
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

    /// 关闭专属socket读写对即可唤醒poll，不依赖再次连接端口；每个连接总时限也限制错误清理。
    pub(super) fn shutdown(&mut self) -> Result<(), String> {
        drop(self.cancel.take());
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "本轮HTTP服务线程异常退出")?;
        }
        Ok(())
    }
}

impl Drop for ExternalHttp {
    fn drop(&mut self) {
        // 普通成功/失败由Host显式shutdown并交付错误；构造后未交接的异常展开仍不遗留线程/端口。
        let _ = self.shutdown();
    }
}

fn notify(signal: StepTerminalSignal) {
    // 工作线程只交原有限标量；Native、Driver和正文只能在原主队列重新核权后访问。
    dispatch2::DispatchQueue::main().exec_async(move || {
        eprintln!(
            "external_http event=terminal_signal_dispatched step_index={}",
            signal.index,
        );
        hosted_step_terminal(signal);
    });
}

/// poll同时等待原监听器与专属取消socket；无定时轮询、备用端口或重放浏览器动作。
fn serve(
    listener: &TcpListener,
    cancellation: &UnixStream,
    host: &str,
    path: &str,
    nonce: &str,
    receipt: &Mutex<Receipt>,
) -> Result<(), String> {
    let hidden_path = format!("{path}/pagehide");
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Omni HTTP验收</title><p>本轮HTTP页面将自动关闭。</p>\
        <script nonce='{nonce}'>let closing=false;\
        addEventListener('pagehide',event=>{{if(closing&&event.isTrusted&&!event.persisted)\
        navigator.sendBeacon('{hidden_path}',new Blob(['standard-self-close|trusted=true|persisted=false'],{{type:'text/plain'}}));}},{{once:true}});\
        addEventListener('load',()=>{{closing=true;window.close();}},{{once:true}});</script>"
    );
    for _ in 0..16 {
        // 只有本两个自有FD进入等待，取消端HUP在没有任何浏览器连接时也能结束线程。
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
            // SAFETY: 两个FD均由本工作线程的借用保持，数组长度与poll的nfds完全一致。
            if unsafe { libc::poll(waits.as_mut_ptr(), waits.len() as libc::nfds_t, -1) } >= 0 {
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err("本轮HTTP资源等待失败".into());
            }
        }
        if waits[1].revents != 0 {
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
            // 原循环最多16条连接，记录accept事实即可区分未打开、空预连接和有请求但未卸载。
            let mut state = receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
            state.connections += 1;
        }
        stream
            .set_nonblocking(false)
            .map_err(|_| "无法设置HTTP连接阻塞期限")?;
        let started = Instant::now();
        let Some(request) = read_request(&mut stream, started)? else {
            // 浏览器可先建立连接而不发送HTTP；只关闭这条空预连接，不把它当作页面GET或重放导航。
            // 它仍占原16条连接额度，后继真实请求继续遵守同一监听器与Host三秒终点期限。
            receipt
                .lock()
                .map_err(|_| "HTTP交接事实已poison")?
                .empty_connections += 1;
            continue;
        };
        if request.host != host {
            return Err("本轮HTTP的目标身份不符".into());
        }
        if request.method == "GET" && request.path == path && request.body.is_empty() {
            {
                let mut state = receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
                if !state.armed || state.get_received {
                    return Err("HTTP页面请求未登记或被重复打开".into());
                }
                state.nonce_get_received = true;
            }
            eprintln!("external_http event=nonce_get_received");
            respond(
                &mut stream,
                started,
                "200 OK",
                "text/html; charset=utf-8",
                &page,
                nonce,
            )?;
            receipt
                .lock()
                .map_err(|_| "HTTP交接事实已poison")?
                .get_received = true;
            eprintln!("external_http event=page_served");
        } else if request.method == "POST" && request.path == hidden_path && request.body == RECEIPT
        {
            {
                let mut state = receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
                if !state.get_received {
                    return Err("HTTP卸载回执缺少原页面GET".into());
                }
                state.pagehide_received = true;
            }
            eprintln!("external_http event=trusted_pagehide_received persisted=false");
            respond(
                &mut stream,
                started,
                "204 No Content",
                "text/plain",
                "",
                nonce,
            )?;
            return Ok(()); // 只证明自有页面已卸载，不声称Chrome标签/FrameTree或历史数据库销毁。
        } else if request.method == "GET"
            && request.path == "/favicon.ico"
            && request.body.is_empty()
        {
            respond(
                &mut stream,
                started,
                "204 No Content",
                "text/plain",
                "",
                nonce,
            )?;
        } else {
            return Err("本轮HTTP收到未声明的请求".into());
        }
    }
    Err("本轮HTTP连接数量超过有限交接范围".into())
}

struct Request {
    method: String,
    path: String,
    host: String,
    body: Vec<u8>,
}

/// 自有页面的HTTP/1.1请求仅有8KiB头和128byte回执；拒绝分块/重复长度，不形成通用服务器。
fn read_request(stream: &mut TcpStream, started: Instant) -> Result<Option<Request>, String> {
    let mut bytes = Vec::with_capacity(1024);
    let header_end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() >= 8192 {
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
                // 尚无HTTP字节的闲置socket不构成请求；一旦收到任意字节，原总期限失败仍须上报。
                return Ok(None);
            }
            Err(_) => return Err("本轮HTTP请求读取失败或超时".into()),
        };
        if count == 0 {
            if bytes.is_empty() {
                return Ok(None); // 未被浏览器使用的预连接可直接关闭，不能伪造HTTP完成事实。
            }
            return Err("本轮HTTP请求在头部结束前断开".into());
        }
        bytes.extend_from_slice(&page[..count]);
    };
    if header_end > 8192 {
        return Err("本轮HTTP请求头超过边界".into());
    }
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
        return Err("本轮HTTP出现声明外的尾部".into());
    }
    while body.len() < length {
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

fn remaining(started: Instant) -> Result<Duration, String> {
    Duration::from_secs(1)
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| "本轮HTTP连接超过一秒期限".into())
}

/// 固定响应关闭连接，不留下浏览器keep-alive；每次实际write都继承同一总期限。
fn respond(
    stream: &mut TcpStream,
    started: Instant,
    status: &str,
    mime: &str,
    body: &str,
    nonce: &str,
) -> Result<(), String> {
    let bytes = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'nonce-{nonce}'; connect-src 'self'\r\n\r\n{body}",
        body.len()
    );
    let mut pending = bytes.as_bytes();
    while !pending.is_empty() {
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
