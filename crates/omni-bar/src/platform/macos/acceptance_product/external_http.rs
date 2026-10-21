//! 本轮HTTP交付只有一个监听器、一个工作线程和一个终点；不控制浏览器或持有任何文稿。

use super::{StepTerminalSignal, hosted_step_terminal};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 这里保存服务器实际收讫事实；迟到观察读取同一终态，不靠点击排队或openURL返回值通过。
struct Receipt {
    armed: bool,
    connections: u8,
    empty_connections: u8,
    nonce_get_received: bool,
    response_written: bool,
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
    /// 实际端口由内核分配，随机nonce只属于本轮交付；不注册协议或修改默认浏览器。
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
                // 终点先归还端口和取消读端；通知不是工作线程仍占监听器的替代完成标志。
                drop(listener);
                drop(cancellation);
                // 这里只报告本服务的有界终态；响应写完不代表浏览器标签或历史已清理。
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
        // 只记录本构造器刚生成的loopback身份，便于本轮精准核对；这里没有用户URL或正文。
        eprintln!("external_http event=listener_created url={url}");
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

    /// 必须在原鼠标动作之前登记；服务器不接受未授权的预取或旧请求。
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

    /// 只证明原GET及204完整写入；不从打开BOOL推断浏览器呈现、标签销毁或历史清理。
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

    /// 原Host期限只读本网络拥有者的实际进度；相对导航前的Body已释放，不能诊断它冒充HTTP等待。
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
        }); // 日志I/O在mutex释放后执行，只复制有限计数与本服务的固定错误，不读取页面正文。
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
    receipt: &Mutex<Receipt>,
) -> Result<(), String> {
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
            // 原循环最多16条连接，记录accept事实即可区分未交付、空预连接和实际GET。
            let mut state = receipt.lock().map_err(|_| "HTTP交接事实已poison")?;
            state.connections += 1;
        }
        stream
            .set_nonblocking(false)
            .map_err(|_| "无法设置HTTP连接阻塞期限")?;
        let started = Instant::now();
        let Some(request) = read_request(&mut stream, started)? else {
            // 浏览器可先建立连接而不发送HTTP；只关闭这条空预连接，不把它当作GET或重放导航。
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
                if !state.armed || state.response_written {
                    return Err("HTTP交付请求未登记或已结束".into());
                }
                state.nonce_get_received = true;
            }
            eprintln!("external_http event=nonce_get_received");
            // 204不创建自有HTML/JS，不再把外部浏览器的脚本关闭权限当作产品交付门。
            // 写入沿本连接原一秒总期限；中途失败时保留真实错误，不能签发完成收据。
            respond_no_content(&mut stream, started)?;
            receipt
                .lock()
                .map_err(|_| "HTTP交接事实已poison")?
                .response_written = true;
            eprintln!("external_http event=response_written status=204 browser_cleanup=unverified");
            return Ok(()); // 连接随后drop；worker先归还监听器，再交付原Host终点，不声称标签关闭。
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

/// HTTP/1.1请求仅有8KiB头和128byte有界载荷；GET只允许空正文，拒绝分块或重复长度。
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

/// 204仅写响应头；不发送正文或Content-Length，实际连接关闭和worker归还各有原owner。
fn respond_no_content(stream: &mut TcpStream, started: Instant) -> Result<(), String> {
    // 成功响应需要真实Date；UTC转换只使用本栈缓冲，不改变进程locale或引入日期缓存。
    // SAFETY: time允许空输出指针；gmtime_r只写本栈已分配的tm，成功后才读取初始化结果。
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut utc = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: gmtime_r同步借用本栈时间戳和tm空间；只有非空成功返回后才读取输出。
    if now == -1 || unsafe { libc::gmtime_r(&now, utc.as_mut_ptr()) }.is_null() {
        return Err("无法取得本轮HTTP响应UTC时间".into());
    }
    // SAFETY: 上方gmtime_r已返回非空，tm已由系统完整初始化；不将任何指针借到写入阶段。
    let utc = unsafe { utc.assume_init() };
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
        // Date生成也包含在原started起点内；每次部分write继续扣同一秒，不续期。
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

        // 只经生产URL发送真实请求，不登记AppKit观察者，也不直接调用serve或修改收据。
        client.write_all(request.as_bytes()).expect("实际写入原GET");
        let mut response = Vec::new();
        let read = client.read_to_end(&mut response);
        let joined = server.shutdown();
        drop(client); // 先结束双方真实资源，再断言响应，失败或panic仍由原Drop兜底释放。
        read.expect("服务端写完并实际关闭连接");
        joined.expect("原worker实际返回并join");
        server.verify().expect("原真实GET及完整响应已完成");

        // 从实际网络字节判定协议：204在头尾即结束，不能夹带HTML、JS或长度/分块正文。
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

        // 请求字节完全合法，但没有原arm；不能以收到GET或关闭socket代替业务登记。
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

    #[test]
    fn armed_listener_without_a_request_can_be_cancelled_and_joined() {
        let mut server = ExternalHttp::new().expect("建立真实本机监听器");
        server.arm().expect("登记本次交付");
        let target = server.url().strip_prefix("http://").expect("本机HTTP URL");
        let (address, _) = target.split_once('/').expect("完整nonce路径");
        let address = address.to_owned();

        // 不建立TCP连接或补发请求；取消必须经生产专属Unix通道唤醒原poll并结束worker。
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
