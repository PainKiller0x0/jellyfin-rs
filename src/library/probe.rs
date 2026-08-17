use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde::{Deserialize, Deserializer};

const LOCAL_FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_FFPROBE_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_FFPROBE_ANALYZE_DURATION_MICROS: &str = "30000000";
const LOCAL_FFPROBE_PROBE_SIZE_BYTES: &str = "100000000";
const REMOTE_FFPROBE_ANALYZE_DURATION_MICROS: &str = "5000000";
const REMOTE_FFPROBE_PROBE_SIZE_BYTES: &str = "10000000";
const REMOTE_FFPROBE_RW_TIMEOUT_MICROS: &str = "5000000";
const REMOTE_FFPROBE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const REMOTE_PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REMOTE_REDIRECT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(6);
const REMOTE_PROBE_FAILURE_TTL: Duration = Duration::from_secs(300);

static REMOTE_PROBE_FAILURES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

enum ProbeFailure {
    EndpointUnavailable,
    Failed,
}

struct Ipv4ProbeProxy {
    url: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct MediaProbe {
    pub runtime_ticks: Option<i64>,
    pub size_bytes: Option<i64>,
    pub container: Option<String>,
    pub video_3d_format: Option<String>,
    pub audio_metadata: ProbedAudioMetadata,
    pub streams: Vec<ProbedStream>,
    pub chapters: Vec<ProbedChapter>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbedAudioMetadata {
    pub title: Option<String>,
    pub forced_sort_name: Option<String>,
    pub album: Option<String>,
    pub overview: Option<String>,
    pub production_year: Option<i64>,
    pub premiere_date: Option<String>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub series_name: Option<String>,
    pub artists: Vec<String>,
    pub album_artists: Vec<String>,
    pub composers: Vec<String>,
    pub conductors: Vec<String>,
    pub lyricists: Vec<String>,
    pub writers: Vec<String>,
    pub arrangers: Vec<String>,
    pub engineers: Vec<String>,
    pub mixers: Vec<String>,
    pub remixers: Vec<String>,
    pub narrators: Vec<String>,
    pub illustrators: Vec<String>,
    pub lyrics: Option<String>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub provider_ids: Vec<(String, String)>,
}

#[derive(Clone, Default)]
pub struct ProbedStream {
    pub stream_index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub codec_tag: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub comment: Option<String>,
    pub bit_rate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub aspect_ratio: Option<String>,
    pub average_frame_rate: Option<f64>,
    pub real_frame_rate: Option<f64>,
    pub reference_frame_rate: Option<f64>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub ref_frames: Option<i64>,
    pub is_interlaced: bool,
    pub is_avc: Option<bool>,
    pub is_anamorphic: Option<bool>,
    pub pixel_format: Option<String>,
    pub level: Option<i64>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub time_base: Option<String>,
    pub codec_time_base: Option<String>,
    pub nal_length_size: Option<String>,
    pub rotation: Option<i64>,
    pub video_range: Option<String>,
    pub video_range_type: Option<String>,
    pub hdr10_plus_present_flag: Option<bool>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    pub is_original: Option<bool>,
}

pub struct ProbedChapter {
    pub start_position_ticks: i64,
    pub name: String,
}

pub fn probe_media(path: &Path) -> Option<MediaProbe> {
    let remote_url = remote_probe_url(path);
    let mut redirected_url = remote_url
        .as_ref()
        .and_then(remote_probe_preferred_redirect);
    let mut tried_redirected_url = false;
    if let Some(url) = redirected_url.as_ref() {
        tracing::debug!(
            "probing remote media after redirect resolution: {} -> {}",
            remote_url
                .as_ref()
                .map(redacted_probe_url)
                .unwrap_or_else(|| path.to_string_lossy().to_string()),
            redacted_probe_url(url)
        );
        let redirected_path = Path::new(url.as_str());
        tried_redirected_url = true;
        if let Ok(probe) = probe_media_once(redirected_path, Some(url)) {
            return Some(probe);
        }
    }

    match probe_media_once(path, remote_url.as_ref()) {
        Ok(probe) => Some(probe),
        Err(ProbeFailure::EndpointUnavailable) => None,
        Err(ProbeFailure::Failed) => {
            let redirected_url = redirected_url.take().or_else(|| {
                remote_url
                    .as_ref()
                    .and_then(remote_probe_preferred_redirect)
            })?;
            if tried_redirected_url {
                return None;
            }
            tracing::debug!(
                "retrying remote media probe after redirect resolution: {} -> {}",
                remote_url
                    .as_ref()
                    .map(redacted_probe_url)
                    .unwrap_or_else(|| path.to_string_lossy().to_string()),
                redacted_probe_url(&redirected_url)
            );
            let redirected_path = Path::new(redirected_url.as_str());
            probe_media_once(redirected_path, Some(&redirected_url)).ok()
        }
    }
}

pub fn probe_video_media(
    path: &Path,
    video_type: Option<&str>,
    iso_type: Option<&str>,
) -> Option<MediaProbe> {
    if video_type == Some("Iso") && iso_type == Some("BluRay") {
        return probe_media(&bluray_iso_input_path(path));
    }
    let Some(video_type @ ("Dvd" | "BluRay")) = video_type else {
        return probe_media(path);
    };
    let plan = crate::library::disc::probe_plan(path, video_type)?;
    let first = plan.files.first()?;
    let mut probe = probe_media(first)?;

    if video_type == "Dvd" {
        let mut runtime_ticks = probe.runtime_ticks;
        for file in plan.files.iter().skip(1) {
            let part = probe_media(file)?;
            runtime_ticks = runtime_ticks
                .zip(part.runtime_ticks)
                .map(|(total, part)| total.saturating_add(part));
        }
        probe.runtime_ticks = runtime_ticks;
    } else {
        probe.runtime_ticks = plan.runtime_ticks.or(probe.runtime_ticks);
        if !plan.chapter_ticks.is_empty() {
            probe.chapters = plan
                .chapter_ticks
                .into_iter()
                .enumerate()
                .map(|(index, start_position_ticks)| ProbedChapter {
                    start_position_ticks,
                    name: format!("Chapter {}", index + 1),
                })
                .collect();
        }
        if !plan.streams.is_empty() {
            let ffmpeg_video = probe
                .streams
                .iter()
                .find(|stream| stream.stream_type == "Video")
                .cloned();
            let mut streams = plan
                .streams
                .into_iter()
                .enumerate()
                .map(|(index, stream)| probed_stream_from_disc(index as i64, stream))
                .collect::<Vec<_>>();
            if let (Some(ffmpeg), Some(bluray)) = (
                ffmpeg_video,
                streams
                    .iter_mut()
                    .find(|stream| stream.stream_type == "Video"),
            ) {
                // Jellyfin rebuilds the BDInfo stream list, then fills fields
                // BDInfo lacks from ffprobe's first playable m2ts stream.
                bluray.codec = ffmpeg.codec;
                bluray.bit_rate = bluray.bit_rate.or(ffmpeg.bit_rate);
                bluray.width = bluray.width.or(ffmpeg.width);
                bluray.height = bluray.height.or(ffmpeg.height);
                bluray.color_range = ffmpeg.color_range;
                bluray.color_space = ffmpeg.color_space;
                bluray.color_transfer = ffmpeg.color_transfer;
                bluray.color_primaries = ffmpeg.color_primaries;
                bluray.bit_depth = bluray.bit_depth.or(ffmpeg.bit_depth);
                bluray.pixel_format = ffmpeg.pixel_format;
            }
            probe.streams = streams;
        }
    }

    Some(probe)
}

fn bluray_iso_input_path(path: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("bluray:{}", path.to_string_lossy()))
}

fn probed_stream_from_disc(index: i64, stream: crate::library::disc::DiscStream) -> ProbedStream {
    ProbedStream {
        stream_index: index,
        stream_type: stream.stream_type,
        codec: Some(stream.codec),
        language: stream.language,
        bit_rate: stream.bit_rate,
        width: stream.width,
        height: stream.height,
        average_frame_rate: stream.average_frame_rate,
        real_frame_rate: stream.average_frame_rate,
        reference_frame_rate: stream.average_frame_rate,
        channels: stream.channels,
        channel_layout: stream.channel_layout,
        sample_rate: stream.sample_rate,
        bit_depth: stream.bit_depth,
        is_interlaced: stream.is_interlaced,
        ..Default::default()
    }
}

fn remote_probe_preferred_redirect(url: &reqwest::Url) -> Option<reqwest::Url> {
    if !remote_probe_uses_ipv4_proxy(url) {
        return None;
    }
    let redirected_url = resolve_remote_probe_redirect(url)?;
    (redirected_url.as_str() != url.as_str()).then_some(redirected_url)
}

fn probe_media_once(
    path: &Path,
    remote_url: Option<&reqwest::Url>,
) -> Result<MediaProbe, ProbeFailure> {
    let mut proxy = None;
    let mut http_proxy = None;
    if let Some(url) = remote_url {
        if !remote_probe_endpoint_available(url) {
            return Err(ProbeFailure::EndpointUnavailable);
        }
        if remote_probe_uses_ipv4_proxy(url) {
            let ipv4_proxy = Ipv4ProbeProxy::start().ok_or(ProbeFailure::EndpointUnavailable)?;
            http_proxy = Some(ipv4_proxy.url.clone());
            proxy = Some(ipv4_proxy);
        }
    }
    let output = run_ffprobe(path, remote_url.is_some(), http_proxy.as_deref())
        .ok_or(ProbeFailure::Failed)?;
    drop(proxy);
    let response = match serde_json::from_slice::<FfprobeResponse>(&output) {
        Ok(response) => response,
        Err(error) => {
            if remote_url.is_some() {
                tracing::debug!(
                    "failed to parse remote ffprobe output for {}: {error}",
                    redacted_probe_path(path)
                );
            } else {
                tracing::warn!(
                    "failed to parse ffprobe output for {}: {error}",
                    redacted_probe_path(path)
                );
            }
            return Err(ProbeFailure::Failed);
        }
    };
    Ok(media_probe_from_ffprobe_response(response))
}

fn run_ffprobe(path: &Path, is_remote: bool, http_proxy: Option<&str>) -> Option<Vec<u8>> {
    let ffprobe =
        std::env::var("JELLYFIN_RS_FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_string());
    let analyze_duration = std::env::var("JELLYFIN_RS_FFPROBE_ANALYZE_DURATION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ffprobe_default_analyze_duration(is_remote).to_string());
    let probe_size = std::env::var("JELLYFIN_RS_FFPROBE_PROBE_SIZE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ffprobe_default_probe_size(is_remote).to_string());

    let mut command = Command::new(ffprobe);
    command
        .arg("-v")
        .arg("warning")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg("-show_chapters");
    if ffprobe_scans_frames(is_remote) {
        command
            .arg("-show_frames")
            .arg("-read_intervals")
            .arg("%+#1");
    }
    if analyze_duration != "0" {
        command.arg("-analyzeduration").arg(analyze_duration);
    }
    if probe_size != "0" {
        command.arg("-probesize").arg(probe_size);
    }
    if is_remote {
        command
            .arg("-rw_timeout")
            .arg(REMOTE_FFPROBE_RW_TIMEOUT_MICROS)
            .arg("-user_agent")
            .arg(REMOTE_FFPROBE_USER_AGENT);
        if let Some(http_proxy) = http_proxy {
            command.arg("-http_proxy").arg(http_proxy);
        }
    }

    let mut child = match command
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            if is_remote {
                tracing::debug!(
                    "failed to start remote ffprobe for {}: {error}",
                    redacted_probe_path(path)
                );
            } else {
                tracing::warn!(
                    "failed to start ffprobe for {}: {error}",
                    redacted_probe_path(path)
                );
            }
            return None;
        }
    };

    let output = {
        let timeout = ffprobe_timeout(is_remote);
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break child.wait_with_output().ok()?,
                Ok(None) => {
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        if is_remote {
                            tracing::debug!(
                                "remote ffprobe timed out for: {}",
                                redacted_probe_path(path)
                            );
                        } else {
                            tracing::warn!("ffprobe timed out for: {}", redacted_probe_path(path));
                        }
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to wait for ffprobe on {}: {error}",
                        redacted_probe_path(path)
                    );
                    return None;
                }
            }
        }
    };
    if !output.status.success() {
        if is_remote {
            tracing::debug!(
                "remote ffprobe failed for {}: {}",
                redacted_probe_path(path),
                truncated_probe_stderr(&output.stderr)
            );
        } else {
            tracing::warn!(
                "ffprobe failed for {}: {}",
                redacted_probe_path(path),
                truncated_probe_stderr(&output.stderr)
            );
        }
        return None;
    }
    let output = output.stdout;
    Some(output)
}

fn ffprobe_timeout(is_remote: bool) -> Duration {
    if is_remote {
        REMOTE_FFPROBE_TIMEOUT
    } else {
        LOCAL_FFPROBE_TIMEOUT
    }
}

fn remote_probe_uses_ipv4_proxy(url: &reqwest::Url) -> bool {
    url.host_str()
        .is_none_or(|host| host.parse::<Ipv4Addr>().is_err())
}

fn ffprobe_scans_frames(is_remote: bool) -> bool {
    !is_remote
}

fn ffprobe_default_analyze_duration(is_remote: bool) -> &'static str {
    if is_remote {
        REMOTE_FFPROBE_ANALYZE_DURATION_MICROS
    } else {
        LOCAL_FFPROBE_ANALYZE_DURATION_MICROS
    }
}

fn ffprobe_default_probe_size(is_remote: bool) -> &'static str {
    if is_remote {
        REMOTE_FFPROBE_PROBE_SIZE_BYTES
    } else {
        LOCAL_FFPROBE_PROBE_SIZE_BYTES
    }
}

fn remote_probe_url(path: &Path) -> Option<reqwest::Url> {
    let value = path.to_string_lossy();
    let url = reqwest::Url::parse(&value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

impl Ipv4ProbeProxy {
    fn start() -> Option<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
        listener.set_nonblocking(true).ok()?;
        let address = listener.local_addr().ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let proxy_stop = stop.clone();
        let handle = std::thread::spawn(move || run_ipv4_probe_proxy(listener, proxy_stop));

        Some(Self {
            url: format!("http://{address}"),
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for Ipv4ProbeProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_ipv4_probe_proxy(listener: TcpListener, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((client, _)) => {
                std::thread::spawn(move || {
                    if let Err(error) = handle_ipv4_proxy_connection(client) {
                        tracing::debug!("IPv4 probe proxy connection failed: {error}");
                    }
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                tracing::debug!("IPv4 probe proxy stopped accepting connections: {error}");
                break;
            }
        }
    }
}

fn handle_ipv4_proxy_connection(mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(REMOTE_FFPROBE_TIMEOUT))?;
    client.set_write_timeout(Some(REMOTE_FFPROBE_TIMEOUT))?;

    let (header, extra) = read_proxy_header(&mut client)?;
    let request = ProxyRequest::parse(&header)?;
    let mut upstream = connect_ipv4_upstream(&request.host, request.port)?;
    upstream.set_read_timeout(Some(REMOTE_FFPROBE_TIMEOUT))?;
    upstream.set_write_timeout(Some(REMOTE_FFPROBE_TIMEOUT))?;

    if request.is_connect {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    } else {
        upstream.write_all(&request.forward_header)?;
        if !extra.is_empty() {
            upstream.write_all(&extra)?;
        }
    }

    proxy_bidirectional(client, upstream)
}

struct ProxyRequest {
    host: String,
    port: u16,
    is_connect: bool,
    forward_header: Vec<u8>,
}

impl ProxyRequest {
    fn parse(header: &[u8]) -> io::Result<Self> {
        let text = std::str::from_utf8(header)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proxy header is not UTF-8"))?;
        let request_line = text
            .lines()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
        let mut parts = request_line.split_whitespace();
        let method = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?;
        let target = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing target"))?;
        let version = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP version"))?;

        if method.eq_ignore_ascii_case("CONNECT") {
            let (host, port) = parse_proxy_authority(target, 443)?;
            return Ok(Self {
                host,
                port,
                is_connect: true,
                forward_header: Vec::new(),
            });
        }

        if let Ok(url) = reqwest::Url::parse(target) {
            let host = url
                .host_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing URL host"))?
                .to_string();
            let port = url.port_or_known_default().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing URL default port")
            })?;
            let path = match url.query() {
                Some(query) => format!("{}?{query}", url.path()),
                None => url.path().to_string(),
            };
            return Ok(Self {
                host,
                port,
                is_connect: false,
                forward_header: rewrite_proxy_request_line(method, &path, version, text),
            });
        }

        let host_header = proxy_header_value(text, "host")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Host header"))?;
        let (host, port) = parse_proxy_authority(host_header, 80)?;
        Ok(Self {
            host,
            port,
            is_connect: false,
            forward_header: header.to_vec(),
        })
    }
}

fn read_proxy_header(client: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    const MAX_HEADER_BYTES: usize = 32 * 1024;

    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = client.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before proxy header",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = header_end(&buffer) {
            let extra = buffer.split_off(end);
            return Ok((buffer, extra));
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy header is too large",
            ));
        }
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_proxy_authority(value: &str, default_port: u16) -> io::Result<(String, u16)> {
    let value = value.trim();
    if value.starts_with('[') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPv6 proxy authority is not supported for media probe",
        ));
    }
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|ch| ch.is_ascii_digit()) => {
            (host, port.parse().unwrap_or(default_port))
        }
        _ => (value, default_port),
    };
    if host.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing proxy host",
        ));
    }
    Ok((host.trim().to_string(), port))
}

fn proxy_header_value<'a>(headers: &'a str, wanted: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case(wanted)
            .then(|| value.trim())
    })
}

fn rewrite_proxy_request_line(method: &str, path: &str, version: &str, header: &str) -> Vec<u8> {
    let rest = header
        .split_once("\r\n")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    format!("{method} {path} {version}\r\n{rest}").into_bytes()
}

fn connect_ipv4_upstream(host: &str, port: u16) -> io::Result<TcpStream> {
    let ipv4 = remote_probe_ipv4(host, port).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("no IPv4 address for {host}:{port}"),
        )
    })?;
    TcpStream::connect_timeout(
        &SocketAddr::from((ipv4, port)),
        REMOTE_PROBE_CONNECT_TIMEOUT,
    )
}

fn proxy_bidirectional(client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let client_to_upstream = std::thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });

    let mut upstream_read = upstream;
    let mut client_write = client;
    let result = io::copy(&mut upstream_read, &mut client_write).map(|_| ());
    let _ = client_write.shutdown(Shutdown::Write);
    let _ = client_to_upstream.join();
    result
}

fn resolve_remote_probe_redirect(url: &reqwest::Url) -> Option<reqwest::Url> {
    let ipv4_proxy = Ipv4ProbeProxy::start();
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(REMOTE_REDIRECT_RESOLVE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(REMOTE_FFPROBE_USER_AGENT);
    if let Some(proxy) = ipv4_proxy.as_ref() {
        builder = builder.proxy(reqwest::Proxy::all(&proxy.url).ok()?);
    }
    let client = builder.build().ok()?;

    match resolve_remote_probe_redirect_with_method(&client, url) {
        Ok(url) => url,
        Err(error) => {
            tracing::debug!(
                "failed to resolve remote media probe redirect for {}: {error}",
                redacted_probe_url(url)
            );
            None
        }
    }
}

fn resolve_remote_probe_redirect_with_method(
    client: &reqwest::blocking::Client,
    url: &reqwest::Url,
) -> Result<Option<reqwest::Url>, reqwest::Error> {
    const MAX_REDIRECTS: usize = 5;

    let original = url.clone();
    let mut current = url.clone();
    for _ in 0..MAX_REDIRECTS {
        let response = client
            .get(current.clone())
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()?;
        if let Some(next) = redirect_location(response.url(), &response) {
            current = next;
            continue;
        }
        return Ok((current.as_str() != original.as_str()).then_some(current));
    }
    Ok((current.as_str() != original.as_str()).then_some(current))
}

fn redirect_location(
    base_url: &reqwest::Url,
    response: &reqwest::blocking::Response,
) -> Option<reqwest::Url> {
    if !response.status().is_redirection() {
        return None;
    }
    let location = response.headers().get(reqwest::header::LOCATION)?;
    let location = location.to_str().ok()?;
    base_url.join(location).ok()
}

fn remote_probe_endpoint_available(url: &reqwest::Url) -> bool {
    let Some(key) = remote_probe_endpoint_key(url) else {
        return true;
    };
    if remote_probe_failure_is_fresh(&key) {
        tracing::debug!(
            "remote media probe skipped because endpoint is recently unavailable: {}",
            redacted_probe_url(url)
        );
        return false;
    }

    let Some((host, port)) = remote_probe_socket(url) else {
        return true;
    };
    let Some(ipv4) = remote_probe_ipv4(&host, port) else {
        remember_remote_probe_failure(&key);
        tracing::warn!(
            "remote media probe skipped because endpoint resolved no IPv4 address: {}",
            redacted_probe_url(url)
        );
        return false;
    };

    let address = SocketAddr::from((ipv4, port));
    match TcpStream::connect_timeout(&address, REMOTE_PROBE_CONNECT_TIMEOUT) {
        Ok(_) => {
            clear_remote_probe_failure(&key);
            return true;
        }
        Err(error) => {
            let should_warn = remember_remote_probe_failure(&key);
            if should_warn {
                tracing::warn!(
                    "remote media probe skipped because IPv4 endpoint is unreachable: {} ({error})",
                    redacted_probe_url(url)
                );
            }
        }
    }
    false
}

fn remote_probe_socket(url: &reqwest::Url) -> Option<(String, u16)> {
    Some((url.host_str()?.to_string(), url.port_or_known_default()?))
}

fn remote_probe_ipv4(host: &str, port: u16) -> Option<Ipv4Addr> {
    if let Ok(ipv4) = host.parse::<Ipv4Addr>() {
        return Some(ipv4);
    }
    (host, port)
        .to_socket_addrs()
        .ok()?
        .find_map(|address| match address {
            SocketAddr::V4(address) => Some(*address.ip()),
            SocketAddr::V6(_) => None,
        })
}

fn remote_probe_endpoint_key(url: &reqwest::Url) -> Option<String> {
    let (host, port) = remote_probe_socket(url)?;
    Some(format!(
        "{}://{}:{port}",
        url.scheme(),
        host.to_ascii_lowercase()
    ))
}

fn remote_probe_failures() -> &'static Mutex<HashMap<String, Instant>> {
    REMOTE_PROBE_FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remote_probe_failure_is_fresh(key: &str) -> bool {
    let now = Instant::now();
    remote_probe_failures()
        .lock()
        .ok()
        .and_then(|failures| failures.get(key).copied())
        .is_some_and(|failed_at| now.duration_since(failed_at) < REMOTE_PROBE_FAILURE_TTL)
}

fn remember_remote_probe_failure(key: &str) -> bool {
    let now = Instant::now();
    let Ok(mut failures) = remote_probe_failures().lock() else {
        return true;
    };
    let should_warn = failures
        .get(key)
        .is_none_or(|failed_at| now.duration_since(*failed_at) >= REMOTE_PROBE_FAILURE_TTL);
    failures.insert(key.to_string(), now);
    should_warn
}

fn clear_remote_probe_failure(key: &str) {
    if let Ok(mut failures) = remote_probe_failures().lock() {
        failures.remove(key);
    }
}

fn redacted_probe_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(url) = remote_probe_url(path) {
        return redacted_probe_url(&url);
    }
    value.to_string()
}

fn redacted_probe_url(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn truncated_probe_stderr(stderr: &[u8]) -> String {
    const MAX_LEN: usize = 500;
    let message = String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if message.is_empty() {
        return "no stderr output".to_string();
    }
    if message.chars().count() <= MAX_LEN {
        return message;
    }
    let truncated = message.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

fn media_probe_from_ffprobe_response(response: FfprobeResponse) -> MediaProbe {
    let FfprobeResponse {
        streams,
        frames,
        chapters,
        format,
    } = response;
    let frames_by_stream = frames
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|frame| frame.stream_index.map(|index| (index, frame)))
        .collect::<HashMap<_, _>>();
    let hdr10_plus_streams = frames
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|frame| {
            frame
                .side_data_list
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|side_data| {
                    side_data.side_data_type.as_deref().is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("HDR Dynamic Metadata SMPTE2094-40 (HDR10+)")
                    })
                })
        })
        .filter_map(|frame| frame.stream_index)
        .collect::<std::collections::HashSet<_>>();
    let runtime_ticks = format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .and_then(|duration| duration.parse::<f64>().ok())
        .map(|seconds| (seconds * 10_000_000.0) as i64);
    let size_bytes = format
        .as_ref()
        .and_then(|format| format.size.as_deref())
        .and_then(parse_i64)
        .filter(|size| *size > 0);
    let container = format
        .as_ref()
        .and_then(|format| format.format_name.as_deref())
        .and_then(normalize_probe_container);
    let video_3d_format = format
        .as_ref()
        .and_then(|format| tag_value(format.tags.as_ref(), "stereo_mode"))
        .filter(|mode| mode.eq_ignore_ascii_case("left_right"))
        .map(|_| "FullSideBySide".to_string());
    let audio_metadata = probed_audio_metadata(&streams, format.as_ref());

    MediaProbe {
        runtime_ticks,
        size_bytes,
        container,
        video_3d_format,
        audio_metadata,
        chapters: probed_chapters_from_ffprobe(chapters),
        streams: streams
            .into_iter()
            .filter_map(|stream| {
                let frame = frames_by_stream.get(&stream.index).copied();
                let has_hdr10_plus = hdr10_plus_streams.contains(&stream.index);
                ProbedStream::from_ffprobe(stream, frame, has_hdr10_plus)
            })
            .collect(),
    }
}

fn probed_audio_metadata(
    streams: &[FfprobeStream],
    format: Option<&FfprobeFormat>,
) -> ProbedAudioMetadata {
    // Jellyfin merges the first audio stream tags with the format tags, with
    // format tags taking precedence. Lower-casing here preserves ffprobe's
    // case-insensitive tag semantics for all subsequent lookups.
    let mut tags = HashMap::new();
    if let Some(stream_tags) = streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"))
        .and_then(|stream| stream.tags.as_ref())
    {
        merge_probe_tags(&mut tags, stream_tags);
    }
    if let Some(format_tags) = format.and_then(|format| format.tags.as_ref()) {
        merge_probe_tags(&mut tags, format_tags);
    }
    if tags.is_empty() {
        return ProbedAudioMetadata::default();
    }

    let premiere_date = first_tag(
        &tags,
        &[
            "originaldate",
            "retaildate",
            "retail date",
            "retail_date",
            "date_released",
            "date",
            "creation_time",
        ],
    )
    .and_then(normalize_probe_date);
    let production_year = tag_number(&tags, "date")
        .filter(|year| (1..=9999).contains(year))
        .or_else(|| {
            premiere_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .and_then(|year| year.parse().ok())
        });
    let artists = first_tag(&tags, &["artists"])
        .map(|value| split_distinct(value, &['/', ';'], false, false))
        .unwrap_or_else(|| {
            first_tag(&tags, &["artist"])
                .map(|value| split_distinct(value, &['/', ';', '|', '\\'], true, false))
                .unwrap_or_default()
        });
    let mut album_artists = first_tag(&tags, &["albumartist", "album artist", "album_artist"])
        .map(|value| split_distinct(value, &['/', ';', '|', '\\'], true, false))
        .unwrap_or_default();
    if album_artists.is_empty() {
        album_artists.clone_from(&artists);
    }
    let genres = first_tag(&tags, &["genre"])
        .map(|value| split_distinct(value, &['/', ';', ','], false, false))
        .unwrap_or_default();
    let studios = ["organization", "ensemble", "publisher", "label"]
        .into_iter()
        .filter_map(|key| tag(&tags, key))
        .flat_map(|value| split_distinct(value, &['/', ';', '|', '\\'], false, true))
        .filter(|studio| {
            !artists
                .iter()
                .chain(album_artists.iter())
                .any(|artist| artist.eq_ignore_ascii_case(studio))
        })
        .collect::<Vec<_>>();

    let mut provider_ids = Vec::new();
    for (provider, keys) in [
        (
            "MusicBrainzAlbumArtist",
            &["musicbrainz album artist id", "musicbrainz_albumartistid"][..],
        ),
        (
            "MusicBrainzArtist",
            &["musicbrainz artist id", "musicbrainz_artistid"][..],
        ),
        (
            "MusicBrainzAlbum",
            &["musicbrainz album id", "musicbrainz_albumid"][..],
        ),
        (
            "MusicBrainzReleaseGroup",
            &["musicbrainz release group id", "musicbrainz_releasegroupid"][..],
        ),
        (
            "MusicBrainzTrack",
            &["musicbrainz release track id", "musicbrainz_releasetrackid"][..],
        ),
        (
            "MusicBrainzRecording",
            &["musicbrainz track id", "musicbrainz_trackid"][..],
        ),
    ] {
        if let Some(id) = first_tag(&tags, keys).and_then(first_musicbrainz_id) {
            provider_ids.push((provider.to_string(), id));
        }
    }

    ProbedAudioMetadata {
        title: first_tag(&tags, &["title", "title-eng"]).map(ToString::to_string),
        forced_sort_name: first_tag(&tags, &["sort_name", "title-sort", "titlesort"])
            .map(ToString::to_string),
        album: tag(&tags, "album").map(ToString::to_string),
        overview: first_tag(&tags, &["synopsis", "description", "desc", "comment"])
            .map(ToString::to_string),
        production_year,
        premiere_date,
        index_number: tag_number(&tags, "track"),
        parent_index_number: tag_number(&tags, "disc"),
        series_name: first_tag(&tags, &["series", "show_name", "show"]).map(ToString::to_string),
        artists,
        album_artists,
        composers: split_tag(&tags, "composer"),
        conductors: split_tag(&tags, "conductor"),
        lyricists: split_tag(&tags, "lyricist"),
        writers: split_tag(&tags, "writer"),
        arrangers: split_tag(&tags, "arranger"),
        engineers: split_tag(&tags, "engineer"),
        mixers: split_tag(&tags, "mixer"),
        remixers: split_tag(&tags, "remixer"),
        narrators: split_tag(&tags, "narrator"),
        illustrators: split_tag(&tags, "illustrator"),
        lyrics: first_tag(
            &tags,
            &[
                "syncedlyrics",
                "synced lyrics",
                "lyrics",
                "unsyncedlyrics",
                "unsynced lyrics",
            ],
        )
        .map(ToString::to_string),
        genres,
        studios: distinct_strings(studios),
        provider_ids,
    }
}

fn merge_probe_tags(target: &mut HashMap<String, String>, source: &HashMap<String, String>) {
    for (key, value) in source {
        if let Some(value) = sanitized_tag(value) {
            target.insert(key.trim().to_ascii_lowercase(), value.to_string());
        }
    }
}

fn sanitized_tag(value: &str) -> Option<&str> {
    let value = value.split('\0').next().unwrap_or_default().trim();
    (!value.is_empty()).then_some(value)
}

fn tag<'a>(tags: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    tags.get(&key.to_ascii_lowercase())
        .and_then(|value| sanitized_tag(value))
}

fn first_tag<'a>(tags: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| tag(tags, key))
}

fn tag_number(tags: &HashMap<String, String>, key: &str) -> Option<i64> {
    tag(tags, key)?
        .split(['/', '-', ' '])
        .next()?
        .trim()
        .parse()
        .ok()
}

fn normalize_probe_date(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(date) = crate::util::normalize_yyyy_mm_dd(value) {
        return Some(date);
    }
    let year = value.get(..4)?.parse::<i64>().ok()?;
    (1..=9999)
        .contains(&year)
        .then(|| format!("{year:04}-01-01"))
}

fn split_tag(tags: &HashMap<String, String>, key: &str) -> Vec<String> {
    tag(tags, key)
        .map(|value| split_distinct(value, &['/', ';', '|', '\\'], false, false))
        .unwrap_or_default()
}

fn split_distinct(
    value: &str,
    delimiters: &[char],
    split_featuring: bool,
    allow_comma: bool,
) -> Vec<String> {
    let value = if split_featuring {
        value
            .replace(" featuring ", " | ")
            .replace(" Featuring ", " | ")
            .replace(" feat. ", " | ")
            .replace(" Feat. ", " | ")
    } else {
        value.to_string()
    };
    let delimiters = if allow_comma && !value.chars().any(|ch| delimiters.contains(&ch)) {
        vec![',']
    } else {
        delimiters.to_vec()
    };
    distinct_strings(
        value
            .split(|ch| delimiters.contains(&ch) || ch == '\u{1f}')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect(),
    )
}

fn distinct_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.to_ascii_lowercase()))
        .collect()
}

fn first_musicbrainz_id(value: &str) -> Option<String> {
    value
        .split(['/', ';', '|', '\\', '\u{1f}'])
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToString::to_string)
}

impl ProbedStream {
    fn from_ffprobe(
        stream: FfprobeStream,
        frame: Option<&FfprobeFrame>,
        has_hdr10_plus: bool,
    ) -> Option<Self> {
        let disposition = stream.disposition.as_ref();
        let is_attached_pic =
            disposition.is_some_and(|disposition| disposition.attached_pic.unwrap_or(0) != 0);
        let stream_type = match stream.codec_type.as_deref()? {
            "video" if is_attached_pic => "EmbeddedImage",
            "video" => "Video",
            "audio" => "Audio",
            "subtitle" => "Subtitle",
            "attachment" => "Attachment",
            _ => return None,
        }
        .to_string();
        let language = tag_value(stream.tags.as_ref(), "language");
        let mut title = if matches!(stream_type.as_str(), "Attachment" | "EmbeddedImage") {
            tag_value(stream.tags.as_ref(), "filename")
                .or_else(|| tag_value(stream.tags.as_ref(), "title"))
        } else {
            tag_value(stream.tags.as_ref(), "title")
        };
        let comment = tag_value(stream.tags.as_ref(), "comment");
        if title.is_none() {
            let handler = tag_value(stream.tags.as_ref(), "handler_name");
            let default_handler = match stream_type.as_str() {
                "Audio" => "SoundHandler",
                "Subtitle" => "SubtitleHandler",
                _ => "",
            };
            if handler
                .as_deref()
                .is_some_and(|value| !value.eq_ignore_ascii_case(default_handler))
            {
                title = handler;
            }
        }

        let bit_rate = stream
            .bit_rate
            .as_deref()
            .and_then(parse_i64)
            .or_else(|| tag_value(stream.tags.as_ref(), "BPS").and_then(|value| parse_i64(&value)))
            .or_else(|| bitrate_from_duration_tags(stream.tags.as_ref()));
        let channel_layout = stream
            .channel_layout
            .as_deref()
            .map(parse_channel_layout)
            .filter(|value| !value.is_empty());
        let pixel_format = stream.pixel_format.clone().or_else(|| {
            frame
                .and_then(|frame| frame.pixel_format.as_ref())
                .map(ToString::to_string)
        });
        let bit_depth = parse_bit_depth(
            stream.bits_per_sample,
            stream.bits_per_raw_sample,
            &pixel_format,
        );
        let color_range = stream
            .color_range
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_range.clone()));
        let color_space = stream
            .color_space
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_space.clone()));
        let color_transfer = stream
            .color_transfer
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_transfer.clone()));
        let color_primaries = stream
            .color_primaries
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_primaries.clone()));
        let is_interlaced = stream
            .field_order
            .as_deref()
            .is_some_and(|field_order| !field_order.eq_ignore_ascii_case("progressive"))
            || frame.and_then(|frame| frame.interlaced_frame).unwrap_or(0) != 0;
        let aspect_ratio = aspect_ratio(
            stream.display_aspect_ratio.as_deref(),
            stream.width,
            stream.height,
        );
        let is_anamorphic = is_anamorphic(
            stream.sample_aspect_ratio.as_deref(),
            stream.display_aspect_ratio.as_deref(),
            stream.width,
            stream.height,
        );
        let (video_range, video_range_type) =
            video_range(&stream_type, color_transfer.as_deref(), has_hdr10_plus);

        Some(Self {
            stream_index: stream.index,
            stream_type,
            codec: stream.codec_name,
            profile: stream.profile,
            codec_tag: stream
                .codec_tag_string
                .and_then(|tag| (!tag.trim().is_empty() && !tag.contains("[0]")).then_some(tag)),
            language,
            title,
            comment,
            bit_rate,
            width: stream.width,
            height: stream.height,
            aspect_ratio,
            average_frame_rate: parse_frame_rate(stream.avg_frame_rate.as_deref()),
            real_frame_rate: parse_frame_rate(stream.real_frame_rate.as_deref()),
            reference_frame_rate: parse_frame_rate(stream.real_frame_rate.as_deref()),
            channels: stream.channels,
            channel_layout,
            sample_rate: stream.sample_rate.as_deref().and_then(parse_i64),
            bit_depth,
            ref_frames: stream.refs.filter(|refs| *refs > 0),
            is_interlaced,
            is_avc: stream.is_avc,
            is_anamorphic,
            pixel_format,
            level: stream.level,
            color_range,
            color_space,
            color_transfer,
            color_primaries,
            time_base: stream.time_base,
            codec_time_base: stream.codec_time_base,
            nal_length_size: stream.nal_length_size,
            rotation: frame
                .and_then(|frame| frame.side_data_list.as_deref())
                .and_then(|side_data| side_data.iter().find_map(|data| data.rotation)),
            video_range,
            video_range_type,
            hdr10_plus_present_flag: has_hdr10_plus.then_some(true),
            is_default: disposition.is_some_and(|d| d.default.unwrap_or(0) != 0),
            is_forced: disposition.is_some_and(|d| d.forced.unwrap_or(0) != 0),
            is_hearing_impaired: disposition.is_some_and(|d| d.hearing_impaired.unwrap_or(0) != 0),
            is_original: disposition
                .and_then(|d| d.original)
                .map(|original| original != 0),
        })
    }
}

#[derive(Deserialize)]
struct FfprobeResponse {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    #[serde(default)]
    frames: Option<Vec<FfprobeFrame>>,
    #[serde(default)]
    chapters: Vec<FfprobeChapter>,
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
    tags: Option<HashMap<String, String>>,
}

fn normalize_probe_container(value: &str) -> Option<String> {
    value.split(',').find_map(|format| {
        let format = format.trim().to_ascii_lowercase();
        match format.as_str() {
            "mpegvideo" => Some("mpeg".to_string()),
            "mpegts" => Some("ts".to_string()),
            "matroska" => Some("mkv".to_string()),
            "mov" | "mp4" | "m4a" | "3gp" | "3g2" | "mj2" => Some("mp4".to_string()),
            "" => None,
            _ => Some(format),
        }
    })
}

#[derive(Deserialize)]
struct FfprobeStream {
    index: i64,
    profile: Option<String>,
    codec_name: Option<String>,
    codec_type: Option<String>,
    codec_tag_string: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    display_aspect_ratio: Option<String>,
    sample_aspect_ratio: Option<String>,
    avg_frame_rate: Option<String>,
    #[serde(rename = "r_frame_rate")]
    real_frame_rate: Option<String>,
    channels: Option<i64>,
    channel_layout: Option<String>,
    sample_rate: Option<String>,
    bit_rate: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    bits_per_sample: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    bits_per_raw_sample: Option<i64>,
    refs: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    is_avc: Option<bool>,
    #[serde(rename = "pix_fmt")]
    pixel_format: Option<String>,
    level: Option<i64>,
    field_order: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    time_base: Option<String>,
    codec_time_base: Option<String>,
    nal_length_size: Option<String>,
    disposition: Option<FfprobeDisposition>,
    tags: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct FfprobeDisposition {
    attached_pic: Option<i64>,
    default: Option<i64>,
    forced: Option<i64>,
    hearing_impaired: Option<i64>,
    original: Option<i64>,
}

#[derive(Deserialize)]
struct FfprobeFrame {
    stream_index: Option<i64>,
    #[serde(rename = "pix_fmt")]
    pixel_format: Option<String>,
    interlaced_frame: Option<i64>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    side_data_list: Option<Vec<FfprobeSideData>>,
}

#[derive(Deserialize)]
struct FfprobeSideData {
    side_data_type: Option<String>,
    rotation: Option<i64>,
}

#[derive(Deserialize)]
struct FfprobeChapter {
    start_time: Option<String>,
    tags: Option<HashMap<String, String>>,
}

fn tag_value(tags: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    tags.and_then(|tags| {
        tags.iter()
            .find(|(candidate, value)| {
                candidate.eq_ignore_ascii_case(key) && !value.trim().is_empty()
            })
            .map(|(_, value)| value.clone())
    })
}

fn parse_i64(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok()
}

fn deserialize_optional_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(value) => parse_i64(&value),
        serde_json::Value::Bool(value) => Some(if value { 1 } else { 0 }),
        _ => None,
    })
}

fn deserialize_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(match value {
        serde_json::Value::Bool(value) => Some(value),
        serde_json::Value::Number(number) => number.as_i64().map(|value| value != 0),
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

fn parse_frame_rate(value: Option<&str>) -> Option<f64> {
    let value = value?.trim();
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    if denominator == 0.0 {
        return None;
    }
    let rate = numerator / denominator;
    (rate > 0.0).then_some(rate)
}

fn parse_channel_layout(value: &str) -> String {
    value
        .split_once('(')
        .map(|(layout, _)| layout)
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn parse_bit_depth(
    bits_per_sample: Option<i64>,
    bits_per_raw_sample: Option<i64>,
    pixel_format: &Option<String>,
) -> Option<i64> {
    bits_per_sample
        .filter(|value| *value > 0)
        .or_else(|| bits_per_raw_sample.filter(|value| *value > 0))
        .or_else(|| {
            let fmt = pixel_format.as_deref()?.to_ascii_lowercase();
            for depth in [16_i64, 14, 12, 10, 9] {
                if fmt.contains(&format!("p{depth}")) || fmt.contains(&format!("p{depth}le")) {
                    return Some(depth);
                }
            }
            fmt.starts_with("yuv").then_some(8)
        })
}

fn aspect_ratio(
    display_aspect_ratio: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
) -> Option<String> {
    let ratio = display_aspect_ratio
        .filter(|value| valid_ratio(value))
        .map(ToString::to_string);
    if ratio.is_some() {
        return ratio;
    }
    let (Some(width), Some(height)) = (width, height) else {
        return None;
    };
    if width <= 0 || height <= 0 {
        return None;
    }
    let ratio = width as f64 / height as f64;
    if is_close(ratio, 1.777_777_778, 0.03) {
        Some("16:9".to_string())
    } else if is_close(ratio, 1.333_333_333, 0.05) {
        Some("4:3".to_string())
    } else if is_close(ratio, 1.41, 0.005) {
        Some("1.41:1".to_string())
    } else if is_close(ratio, 1.5, 0.005) {
        Some("1.5:1".to_string())
    } else if is_close(ratio, 1.6, 0.005) {
        Some("1.6:1".to_string())
    } else if is_close(ratio, 1.666_666_667, 0.005) {
        Some("5:3".to_string())
    } else if is_close(ratio, 1.85, 0.02) {
        Some("1.85:1".to_string())
    } else if is_close(ratio, 2.35, 0.025) {
        Some("2.35:1".to_string())
    } else if is_close(ratio, 2.4, 0.025) {
        Some("2.40:1".to_string())
    } else {
        None
    }
}

fn valid_ratio(value: &str) -> bool {
    let Some((width, height)) = value.split_once(':') else {
        return false;
    };
    width.parse::<i64>().is_ok_and(|value| value > 0)
        && height.parse::<i64>().is_ok_and(|value| value > 0)
}

fn is_anamorphic(
    sample_aspect_ratio: Option<&str>,
    display_aspect_ratio: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
) -> Option<bool> {
    if let Some(sar) = sample_aspect_ratio {
        if near_square_sar(sar) {
            return Some(false);
        }
        if sar != "0:1" {
            return Some(true);
        }
    }
    let Some(dar) = display_aspect_ratio.filter(|value| valid_ratio(value)) else {
        return sample_aspect_ratio.or(display_aspect_ratio).map(|_| false);
    };
    let derived = aspect_ratio(None, width, height);
    Some(derived.as_deref().is_some_and(|value| value != dar))
}

fn near_square_sar(value: &str) -> bool {
    let Some((width, height)) = value.split_once(':') else {
        return false;
    };
    let (Ok(width), Ok(height)) = (width.parse::<f64>(), height.parse::<f64>()) else {
        return false;
    };
    if height == 0.0 {
        return false;
    }
    is_close(width / height, 1.0, 0.001)
}

fn is_close(left: f64, right: f64, variance: f64) -> bool {
    (left - right).abs() <= variance
}

fn bitrate_from_duration_tags(tags: Option<&HashMap<String, String>>) -> Option<i64> {
    let seconds = tag_value(tags, "DURATION").and_then(|value| parse_duration_seconds(&value));
    let bytes = tag_value(tags, "NUMBER_OF_BYTES").and_then(|value| parse_i64(&value));
    let (Some(seconds), Some(bytes)) = (seconds, bytes) else {
        return None;
    };
    (seconds >= 1.0).then_some((bytes as f64 * 8.0 / seconds) as i64)
}

fn parse_duration_seconds(value: &str) -> Option<f64> {
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn video_range(
    stream_type: &str,
    color_transfer: Option<&str>,
    has_hdr10_plus: bool,
) -> (Option<String>, Option<String>) {
    if stream_type != "Video" {
        return (None, None);
    }
    if color_transfer.is_some_and(|value| value.eq_ignore_ascii_case("smpte2084")) {
        return (
            Some("HDR".to_string()),
            Some(if has_hdr10_plus { "HDR10Plus" } else { "HDR10" }.to_string()),
        );
    }
    if color_transfer.is_some_and(|value| value.eq_ignore_ascii_case("arib-std-b67")) {
        return (Some("HDR".to_string()), Some("HLG".to_string()));
    }
    (Some("SDR".to_string()), Some("SDR".to_string()))
}

fn probed_chapters_from_ffprobe(chapters: Vec<FfprobeChapter>) -> Vec<ProbedChapter> {
    chapters
        .into_iter()
        .enumerate()
        .filter_map(|(index, chapter)| {
            let seconds = chapter.start_time.as_deref()?.parse::<f64>().ok()?;
            let start_position_ticks = (seconds * 1000.0).round() as i64 * 10_000;
            let mut name = tag_value(chapter.tags.as_ref(), "title").unwrap_or_default();
            if name.trim().is_empty() || parse_duration_seconds(&name).is_some() {
                name = format!("Chapter {}", index + 1);
            }
            Some(ProbedChapter {
                start_position_ticks,
                name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_probe_extracts_format_size() {
        let response: FfprobeResponse = serde_json::from_str(
            r#"{
                "format": {
                    "duration": "12.500000",
                    "size": "987654321"
                },
                "streams": []
            }"#,
        )
        .unwrap();

        let probe = media_probe_from_ffprobe_response(response);

        assert_eq!(probe.runtime_ticks, Some(125_000_000));
        assert_eq!(probe.size_bytes, Some(987_654_321));
    }

    #[test]
    fn bluray_iso_uses_ffmpeg_bluray_input_protocol() {
        assert_eq!(
            bluray_iso_input_path(Path::new("/media/Movie.iso")).to_string_lossy(),
            "bluray:/media/Movie.iso"
        );
    }

    #[test]
    fn media_probe_extracts_full_sbs_stereo_mode_like_jellyfin() {
        let response: FfprobeResponse = serde_json::from_str(
            r#"{
                "format": {"tags": {"STEREO_MODE": "left_right"}},
                "streams": []
            }"#,
        )
        .unwrap();

        let probe = media_probe_from_ffprobe_response(response);

        assert_eq!(probe.video_3d_format.as_deref(), Some("FullSideBySide"));
    }

    #[test]
    fn media_probe_normalizes_audio_tags_like_jellyfin() {
        let response: FfprobeResponse = serde_json::from_str(
            r#"{
                "format": {
                    "tags": {
                        "TITLE": "Format Title",
                        "ALBUM": "Album One",
                        "ARTIST": "Artist One feat. Artist Two",
                        "ALBUM_ARTIST": "Album Artist",
                        "COMPOSER": "Composer One;Composer Two",
                        "GENRE": "Rock,Pop",
                        "LABEL": "Label One",
                        "TRACK": "3/12",
                        "DISC": "2/3",
                        "DATE": "2024-07-09",
                        "MUSICBRAINZ_ALBUMID": "album-id/ignored-id",
                        "MUSICBRAINZ_TRACKID": "recording-id"
                    }
                },
                "streams": [{
                    "index": 0,
                    "codec_type": "audio",
                    "tags": {"title": "Stream Title", "narrator": "Narrator One"}
                }]
            }"#,
        )
        .unwrap();

        let metadata = media_probe_from_ffprobe_response(response).audio_metadata;

        assert_eq!(metadata.title.as_deref(), Some("Format Title"));
        assert_eq!(metadata.album.as_deref(), Some("Album One"));
        assert_eq!(metadata.artists, ["Artist One", "Artist Two"]);
        assert_eq!(metadata.album_artists, ["Album Artist"]);
        assert_eq!(metadata.composers, ["Composer One", "Composer Two"]);
        assert_eq!(metadata.narrators, ["Narrator One"]);
        assert_eq!(metadata.genres, ["Rock", "Pop"]);
        assert_eq!(metadata.studios, ["Label One"]);
        assert_eq!(metadata.index_number, Some(3));
        assert_eq!(metadata.parent_index_number, Some(2));
        assert_eq!(metadata.premiere_date.as_deref(), Some("2024-07-09"));
        assert_eq!(metadata.production_year, Some(2024));
        assert!(
            metadata
                .provider_ids
                .contains(&("MusicBrainzAlbum".to_string(), "album-id".to_string()))
        );
        assert!(metadata.provider_ids.contains(&(
            "MusicBrainzRecording".to_string(),
            "recording-id".to_string()
        )));
    }

    #[test]
    fn media_probe_extracts_chapters_like_jellyfin() {
        let response: FfprobeResponse = serde_json::from_str(
            r#"{
                "chapters": [
                    {"start_time": "0.000000", "tags": {"title": "00:00:00.000"}},
                    {"start_time": "12.345600", "tags": {"title": "Opening"}}
                ],
                "streams": []
            }"#,
        )
        .unwrap();

        let probe = media_probe_from_ffprobe_response(response);

        assert_eq!(probe.chapters.len(), 2);
        assert_eq!(probe.chapters[0].start_position_ticks, 0);
        assert_eq!(probe.chapters[0].name, "Chapter 1");
        assert_eq!(probe.chapters[1].start_position_ticks, 123_460_000);
        assert_eq!(probe.chapters[1].name, "Opening");
    }

    #[test]
    fn media_probe_extracts_attachment_streams_like_jellyfin() {
        let response: FfprobeResponse = serde_json::from_str(
            r#"{
                "streams": [
                    {
                        "index": 0,
                        "codec_type": "video",
                        "codec_name": "h264"
                    },
                    {
                        "index": 5,
                        "codec_type": "attachment",
                        "codec_name": "ttf",
                        "codec_tag_string": "[0][0][0][0]",
                        "tags": {
                            "filename": "Font.ttf",
                            "mimetype": "application/x-truetype-font",
                            "comment": "subtitle font"
                        }
                    },
                    {
                        "index": 6,
                        "codec_type": "video",
                        "codec_name": "mjpeg",
                        "disposition": { "attached_pic": 1 },
                        "tags": {
                            "filename": "cover.jpg",
                            "comment": "cover"
                        }
                    }
                ]
            }"#,
        )
        .unwrap();

        let probe = media_probe_from_ffprobe_response(response);

        assert_eq!(probe.streams.len(), 3);
        assert_eq!(probe.streams[0].stream_type, "Video");
        assert_eq!(probe.streams[1].stream_type, "Attachment");
        assert_eq!(probe.streams[1].codec.as_deref(), Some("ttf"));
        assert_eq!(probe.streams[1].codec_tag, None);
        assert_eq!(probe.streams[1].title.as_deref(), Some("Font.ttf"));
        assert_eq!(probe.streams[1].comment.as_deref(), Some("subtitle font"));
        assert_eq!(probe.streams[2].stream_type, "EmbeddedImage");
        assert_eq!(probe.streams[2].codec.as_deref(), Some("mjpeg"));
        assert_eq!(probe.streams[2].title.as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn media_probe_ignores_missing_or_invalid_format_size() {
        for size in ["", "0", "-1", "unknown"] {
            let response: FfprobeResponse = serde_json::from_str(&format!(
                r#"{{
                    "format": {{
                        "duration": "1.000000",
                        "size": "{size}"
                    }},
                    "streams": []
                }}"#
            ))
            .unwrap();

            let probe = media_probe_from_ffprobe_response(response);

            assert_eq!(probe.size_bytes, None);
        }
    }

    #[test]
    fn probe_logging_redacts_remote_url_query() {
        assert_eq!(
            redacted_probe_path(Path::new(
                "https://example.test/movie.mkv?token=secret#frag"
            )),
            "https://example.test/movie.mkv"
        );
    }

    #[test]
    fn remote_probe_url_only_accepts_http_urls() {
        assert!(remote_probe_url(Path::new("http://example.test/movie.mkv")).is_some());
        assert!(remote_probe_url(Path::new("https://example.test/movie.mkv")).is_some());
        assert!(remote_probe_url(Path::new("/media/movie.mkv")).is_none());
        assert!(remote_probe_url(Path::new("file:///media/movie.mkv")).is_none());
    }

    #[test]
    fn remote_probe_endpoint_key_uses_known_default_port() {
        let url = reqwest::Url::parse("https://Example.test/movie.mkv?token=secret").unwrap();

        assert_eq!(
            remote_probe_endpoint_key(&url),
            Some("https://example.test:443".to_string())
        );
    }

    #[test]
    fn ipv4_proxy_parses_connect_without_rewriting_host() {
        let request = ProxyRequest::parse(
            b"CONNECT media.example.test:443 HTTP/1.1\r\nHost: media.example.test:443\r\n\r\n",
        )
        .unwrap();

        assert!(request.is_connect);
        assert_eq!(request.host, "media.example.test");
        assert_eq!(request.port, 443);
        assert!(request.forward_header.is_empty());
    }

    #[test]
    fn ipv4_proxy_rewrites_absolute_http_request_to_origin_form() {
        let request =
            ProxyRequest::parse(b"GET http://media.example.test:8080/path/file.mkv?sign=secret HTTP/1.1\r\nHost: media.example.test:8080\r\n\r\n")
                .unwrap();

        assert!(!request.is_connect);
        assert_eq!(request.host, "media.example.test");
        assert_eq!(request.port, 8080);
        assert!(
            std::str::from_utf8(&request.forward_header)
                .unwrap()
                .starts_with(
                    "GET /path/file.mkv?sign=secret HTTP/1.1\r\nHost: media.example.test:8080\r\n"
                )
        );
    }

    #[test]
    fn remote_probe_redirect_resolution_follows_http_302() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::{Arc, Mutex},
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let media_url = format!("http://{address}/media.mp4?token=2");
        let media_url_for_server = media_url.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut served = 0usize;
            while served < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0u8; 1024];
                        let read = stream.read(&mut buffer).unwrap_or_default();
                        let request = String::from_utf8_lossy(&buffer[..read]);
                        server_requests.lock().unwrap().push(request.to_string());
                        if request.contains("/openlist") {
                            write!(
                                stream,
                                "HTTP/1.1 302 Found\r\nLocation: {media_url_for_server}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            )
                            .unwrap();
                        } else {
                            write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            )
                            .unwrap();
                        }
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        let url = reqwest::Url::parse(&format!("http://{address}/openlist?sign=secret")).unwrap();

        let redirected = resolve_remote_probe_redirect(&url).unwrap();

        assert_eq!(redirected.as_str(), media_url);
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        assert!(
            requests
                .iter()
                .all(|request| request.contains("range: bytes=0-0\r\n"))
        );
    }

    #[test]
    fn remote_probe_uses_shorter_ffprobe_timeout() {
        assert!(ffprobe_timeout(true) < ffprobe_timeout(false));
    }

    #[test]
    fn ipv4_literal_remote_probe_connects_directly() {
        let local = reqwest::Url::parse("http://127.0.0.1:8024/media").unwrap();
        let hostname = reqwest::Url::parse("https://media.example.com/video").unwrap();

        assert!(!remote_probe_uses_ipv4_proxy(&local));
        assert!(remote_probe_uses_ipv4_proxy(&hostname));
    }

    #[test]
    fn remote_probe_avoids_expensive_frame_scan() {
        assert!(!ffprobe_scans_frames(true));
        assert!(ffprobe_scans_frames(false));
    }

    #[test]
    fn remote_probe_uses_bounded_analysis_defaults() {
        assert_eq!(ffprobe_default_analyze_duration(true), "5000000");
        assert_eq!(ffprobe_default_probe_size(true), "10000000");
        assert_eq!(ffprobe_default_analyze_duration(false), "30000000");
        assert_eq!(ffprobe_default_probe_size(false), "100000000");
    }

    #[test]
    fn probe_stderr_is_limited() {
        let message = truncated_probe_stderr("错误".repeat(600).as_bytes());
        assert!(message.ends_with("..."));
        assert!(message.chars().count() <= 503);
    }
}
