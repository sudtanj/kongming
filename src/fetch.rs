//! `curl` and `wget` for the `shell` tool.
//!
//! BusyBox's wget has a small built-in TLS that fails on many HTTPS sites and
//! saves to a file by default (nothing can be written in the sandbox), and
//! there is no curl at all. So the kongming binary doubles as both
//! (`kongming curl ...`, or the `curl` and `wget` links in the Docker image):
//! the common options of each over reqwest with rustls, and the body always
//! goes to stdout. Options that only make sense for files are refused with a
//! message saying so; harmless ones (progress, retries, compression) are
//! accepted and ignored.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Method, Url};

const CURL_USAGE: &str = "Usage: curl [options] URL...
kongming's curl: real TLS (rustls), the body always goes to stdout (files can't be written here).
  -s silent  -S show errors  -L follow redirects  -f fail on HTTP >= 400 (--fail-with-body)
  -i include headers  -I headers only (HEAD)  -v verbose  -k insecure  -G data in the query string
  -X METHOD  -H 'Name: value'  -d DATA (@file, @- stdin)  --data-raw  --data-binary  --data-urlencode
  --json DATA  -u USER:PASS  -A AGENT  -e REFERER  -b 'k=v'  -m SECS  --connect-timeout SECS  --max-redirs N
  -o - | -o /dev/null  -w FORMAT: %{http_code} %{content_type} %{url_effective} %{size_download}
                       %{time_total} %{redirect_url} %{remote_ip} \\n
";

const WGET_USAGE: &str = "Usage: wget [options] URL...
kongming's wget: real TLS (rustls), the body always goes to stdout (files can't be written here).
  -q  -O - | -O /dev/null  -S (headers to stderr)  --spider (status only)  --header 'Name: value'
  --post-data DATA  --post-file FILE  --method M --body-data DATA  -U AGENT  -T SECS
  --user U --password P  --max-redirect N  --no-check-certificate  --content-on-error
";

/// Option table: long name, short letter (`NONE` for none), takes a value.
type Spec = (&'static str, char, bool);
const NONE: char = '\0';

const CURL_OPTS: &[Spec] = &[
    ("silent", 's', false),
    ("show-error", 'S', false),
    ("location", 'L', false),
    ("location-trusted", NONE, false),
    ("max-redirs", NONE, true),
    ("request", 'X', true),
    ("header", 'H', true),
    ("data", 'd', true),
    ("data-ascii", NONE, true),
    ("data-binary", NONE, true),
    ("data-raw", NONE, true),
    ("data-urlencode", NONE, true),
    ("json", NONE, true),
    ("get", 'G', false),
    ("include", 'i', false),
    ("head", 'I', false),
    ("output", 'o', true),
    ("remote-name", 'O', false),
    ("remote-name-all", NONE, false),
    ("dump-header", 'D', true),
    ("fail", 'f', false),
    ("fail-with-body", NONE, false),
    ("max-time", 'm', true),
    ("connect-timeout", NONE, true),
    ("user-agent", 'A', true),
    ("referer", 'e', true),
    ("user", 'u', true),
    ("cookie", 'b', true),
    ("insecure", 'k', false),
    ("write-out", 'w', true),
    ("verbose", 'v', false),
    ("url", NONE, true),
    ("help", 'h', false),
    ("version", 'V', false),
    // Accepted and ignored.
    ("compressed", NONE, false),
    ("progress-bar", '#', false),
    ("no-progress-meter", NONE, false),
    ("http1.0", '0', false),
    ("http1.1", NONE, false),
    ("http2", NONE, false),
    ("tlsv1.2", NONE, false),
    ("tlsv1.3", NONE, false),
    ("globoff", 'g', false),
    ("no-buffer", 'N', false),
    ("ipv4", '4', false),
    ("ipv6", '6', false),
    ("cookie-jar", 'c', true),
    ("retry", NONE, true),
    ("retry-delay", NONE, true),
    ("retry-max-time", NONE, true),
    ("retry-connrefused", NONE, false),
    ("retry-all-errors", NONE, false),
    ("no-keepalive", NONE, false),
    ("fail-early", NONE, false),
    ("post301", NONE, false),
    ("post302", NONE, false),
    ("post303", NONE, false),
];

const WGET_OPTS: &[Spec] = &[
    ("quiet", 'q', false),
    ("output-document", 'O', true),
    ("output-file", 'o', true),
    ("append-output", 'a', true),
    ("server-response", 'S', false),
    ("spider", NONE, false),
    ("header", NONE, true),
    ("post-data", NONE, true),
    ("post-file", NONE, true),
    ("method", NONE, true),
    ("body-data", NONE, true),
    ("body-file", NONE, true),
    ("user-agent", 'U', true),
    ("referer", NONE, true),
    ("timeout", 'T', true),
    ("connect-timeout", NONE, true),
    ("dns-timeout", NONE, true),
    ("read-timeout", NONE, true),
    ("no-check-certificate", NONE, false),
    ("user", NONE, true),
    ("http-user", NONE, true),
    ("password", NONE, true),
    ("http-password", NONE, true),
    ("max-redirect", NONE, true),
    ("content-on-error", NONE, false),
    ("save-headers", NONE, false),
    ("help", 'h', false),
    ("version", 'V', false),
    // Accepted and ignored.
    ("proxy", 'Y', true),
    ("continue", 'c', false),
    ("tries", 't', true),
    ("directory-prefix", 'P', true),
    ("verbose", 'v', false),
    ("no-verbose", NONE, false),
    ("no-clobber", NONE, false),
    ("no-directories", NONE, false),
    ("no-parent", NONE, false),
    ("no-host-directories", NONE, false),
    ("no-cache", NONE, false),
    ("no-proxy", NONE, false),
    ("no-dns-cache", NONE, false),
    ("compression", NONE, true),
    ("wait", 'w', true),
    ("waitretry", NONE, true),
    ("retry-connrefused", NONE, false),
    ("inet4-only", '4', false),
    ("inet6-only", '6', false),
    ("progress", NONE, true),
    ("show-progress", NONE, false),
];

/// Entry point of `kongming curl|wget ...`: body to stdout, messages to stderr, returns the exit code.
pub fn main(prog: &str, args: Vec<String>) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{prog}: {e}");
            return 1;
        }
    };
    let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr().lock());
    rt.block_on(run(prog, args, &mut out, &mut err))
}

/// Run one `curl` or `wget` command line (without the program name), writing
/// the body to `out` and messages to `err`; returns the exit code.
pub async fn run(prog: &str, args: Vec<String>, out: &mut impl Write, err: &mut impl Write) -> i32 {
    let mode = if prog == "wget" { Mode::Wget } else { Mode::Curl };
    let code = match parse(mode, args) {
        Ok(o) => o.run(out, err).await,
        Err(e) => {
            let _ = writeln!(err, "{}", e.message);
            e.code
        }
    };
    let _ = out.flush();
    let _ = err.flush();
    code
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Curl,
    Wget,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Curl => "curl",
            Mode::Wget => "wget",
        }
    }
}

/// What an HTTP status >= 400 does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum HttpErrors {
    /// Print the body, exit 0 (curl without -f).
    #[default]
    Ignore,
    /// No body, an error exit (curl -f, wget).
    Fail,
    /// The body, then an error exit (curl --fail-with-body, wget --content-on-error).
    FailWithBody,
}

/// One piece of request body.
#[derive(Debug, Clone, PartialEq)]
enum Data {
    Raw(String),
    /// Read from a file (`-` is stdin); line breaks are dropped unless `binary`, as curl -d does.
    File {
        path: String,
        binary: bool,
    },
    /// `--data-urlencode`: `name=value` with the value percent-encoded.
    Encode(String),
}

#[derive(Debug, Default)]
struct Opts {
    mode: Mode,
    urls: Vec<String>,
    method: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<Data>,
    json: bool,
    get: bool,
    head: bool,
    include: bool,
    discard: bool,
    follow: bool,
    max_redirs: usize,
    errors: HttpErrors,
    silent: bool,
    show_error: bool,
    verbose: bool,
    server_response: bool,
    max_time: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    user_agent: Option<String>,
    user: Option<String>,
    password: Option<String>,
    insecure: bool,
    write_out: Option<String>,
    help: bool,
    version: bool,
}

/// A usage error: the message (already prefixed) and the exit code.
#[derive(Debug)]
struct Exit {
    code: i32,
    message: String,
}

fn exit(code: i32, message: String) -> Exit {
    Exit { code, message }
}

const NO_FILES: &str = "files can't be written here. The output goes to stdout: pipe it instead \
(e.g. `| head -c 3000`), or drop it with";

fn no_files(mode: Mode, path: &str) -> Exit {
    match mode {
        Mode::Curl => exit(23, format!("curl: (23) can't write {path}: {NO_FILES} -o /dev/null")),
        Mode::Wget => exit(1, format!("wget: can't write {path}: {NO_FILES} -O /dev/null")),
    }
}

fn bad_number(mode: Mode, name: &str) -> Exit {
    exit(2, format!("{}: option --{name}: expected a proper numerical parameter", mode.name()))
}

/// getopt-style parsing: clustered short flags (`-sSL`, `-qO-`), attached
/// values (`-XPOST`, `-m5`), `--opt=value` and `--opt value`.
fn parse(mode: Mode, args: Vec<String>) -> Result<Opts, Exit> {
    let table = match mode {
        Mode::Curl => CURL_OPTS,
        Mode::Wget => WGET_OPTS,
    };
    let unknown = |opt: &str| match mode {
        Mode::Curl => exit(2, format!("curl: option {opt}: is unknown\ncurl: try 'curl --help'")),
        Mode::Wget => exit(2, format!("wget: unrecognized option '{opt}'\nTry 'wget --help'")),
    };
    let needs = |opt: &str| exit(2, format!("{}: option {opt}: requires parameter", mode.name()));
    let wget = mode == Mode::Wget;
    let mut o = Opts {
        mode,
        follow: wget,
        max_redirs: if wget { 20 } else { 50 },
        errors: if wget { HttpErrors::Fail } else { HttpErrors::Ignore },
        connect_timeout: wget.then_some(Duration::from_secs(30)),
        ..Opts::default()
    };
    let mut it = args.into_iter();
    let mut only_urls = false;
    while let Some(arg) = it.next() {
        if only_urls || arg == "-" || !arg.starts_with('-') {
            o.urls.push(arg);
            continue;
        }
        if arg == "--" {
            only_urls = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            let Some(&(name, _, takes)) = table.iter().find(|s| s.0 == name) else {
                return Err(unknown(&arg));
            };
            let value = match (takes, inline) {
                (true, Some(v)) => Some(v),
                (true, None) => Some(it.next().ok_or_else(|| needs(&arg))?),
                (false, None) => None,
                (false, Some(_)) => return Err(unknown(&arg)),
            };
            o.apply(name, value)?;
            continue;
        }
        // GNU wget's two-letter short options.
        let wget_no = match arg.as_str() {
            "-nv" => Some("no-verbose"),
            "-nc" => Some("no-clobber"),
            "-nd" => Some("no-directories"),
            "-np" => Some("no-parent"),
            "-nH" => Some("no-host-directories"),
            _ => None,
        };
        if let (true, Some(name)) = (wget, wget_no) {
            o.apply(name, None)?;
            continue;
        }
        let flags = &arg[1..];
        for (i, c) in flags.char_indices() {
            let Some(&(name, _, takes)) = table.iter().find(|s| s.1 == c && c != NONE) else {
                return Err(unknown(&format!("-{c}")));
            };
            if !takes {
                o.apply(name, None)?;
                continue;
            }
            let rest = &flags[i + c.len_utf8()..];
            let value = match rest {
                "" => it.next().ok_or_else(|| needs(&format!("-{c}")))?,
                _ => rest.to_string(),
            };
            o.apply(name, Some(value))?;
            break;
        }
    }
    Ok(o)
}

/// Seconds (fractions allowed); 0 means no limit.
fn secs(mode: Mode, name: &str, v: &str) -> Result<Option<Duration>, Exit> {
    match v.trim().parse::<f64>().ok().and_then(|s| Duration::try_from_secs_f64(s).ok()) {
        Some(d) if d.is_zero() => Ok(None),
        Some(d) => Ok(Some(d)),
        None => Err(bad_number(mode, name)),
    }
}

fn count(mode: Mode, name: &str, v: &str) -> Result<usize, Exit> {
    match v.trim().parse::<i64>() {
        // A negative count means no limit, as in curl.
        Ok(n) => Ok(usize::try_from(n).unwrap_or(usize::MAX)),
        Err(_) => Err(bad_number(mode, name)),
    }
}

fn data_arg(v: String, binary: bool) -> Data {
    match v.strip_prefix('@') {
        Some(path) => Data::File { path: path.to_string(), binary },
        None => Data::Raw(v),
    }
}

impl Opts {
    fn apply(&mut self, name: &str, value: Option<String>) -> Result<(), Exit> {
        let v = value.unwrap_or_default();
        let mode = self.mode;
        match (mode, name) {
            (_, "help") => self.help = true,
            (_, "version") => self.version = true,
            (_, "header") => self.header(&v),
            (_, "user-agent") => self.user_agent = Some(v),
            (_, "referer") => {
                let r = v.trim_end_matches(";auto");
                if !r.is_empty() {
                    self.headers.push(("Referer".into(), r.to_string()));
                }
            }
            (Mode::Curl, "silent") => self.silent = true,
            (Mode::Curl, "show-error") => self.show_error = true,
            (Mode::Curl, "location" | "location-trusted") => self.follow = true,
            (Mode::Curl, "max-redirs") => self.max_redirs = count(mode, name, &v)?,
            (Mode::Curl, "request") => self.method = Some(v),
            (Mode::Curl, "data" | "data-ascii") => self.data.push(data_arg(v, false)),
            (Mode::Curl, "data-binary") => self.data.push(data_arg(v, true)),
            (Mode::Curl, "data-raw") => self.data.push(Data::Raw(v)),
            (Mode::Curl, "data-urlencode") => self.data.push(Data::Encode(v)),
            (Mode::Curl, "json") => {
                self.json = true;
                self.data.push(data_arg(v, true));
            }
            (Mode::Curl, "get") => self.get = true,
            (Mode::Curl, "include") => self.include = true,
            (Mode::Curl, "head") => self.head = true,
            (Mode::Curl, "output") => self.output(&v)?,
            (Mode::Curl, "remote-name" | "remote-name-all") => return Err(no_files(mode, "the remote file name")),
            (Mode::Curl, "dump-header") => match v.as_str() {
                "-" => self.include = true,
                "/dev/null" => {}
                _ => return Err(no_files(mode, &v)),
            },
            (Mode::Curl, "fail") => self.errors = HttpErrors::Fail,
            (Mode::Curl, "fail-with-body") => self.errors = HttpErrors::FailWithBody,
            (Mode::Curl, "max-time") => self.max_time = secs(mode, name, &v)?,
            (Mode::Curl, "connect-timeout") => self.connect_timeout = secs(mode, name, &v)?,
            (Mode::Curl, "user") => match v.split_once(':') {
                Some((u, p)) => {
                    self.user = Some(u.to_string());
                    self.password = Some(p.to_string());
                }
                None => self.user = Some(v),
            },
            // A cookie file (no `=`) is not read.
            (Mode::Curl, "cookie") if v.contains('=') => self.headers.push(("Cookie".into(), v)),
            (Mode::Curl, "insecure") => self.insecure = true,
            (Mode::Curl, "write-out") => self.write_out = Some(v),
            (Mode::Curl, "verbose") => self.verbose = true,
            (Mode::Curl, "url") => self.urls.push(v),
            (Mode::Wget, "quiet") => self.silent = true,
            (Mode::Wget, "output-document") => self.output(&v)?,
            // The log file; messages go to stderr anyway.
            (Mode::Wget, "output-file" | "append-output") if v != "-" && v != "/dev/null" => {
                return Err(no_files(mode, &v));
            }
            (Mode::Wget, "server-response") => self.server_response = true,
            (Mode::Wget, "spider") => self.head = true,
            (Mode::Wget, "post-data" | "body-data") => self.data = vec![Data::Raw(v)],
            (Mode::Wget, "post-file" | "body-file") => self.data = vec![Data::File { path: v, binary: true }],
            (Mode::Wget, "method") => self.method = Some(v.to_uppercase()),
            (Mode::Wget, "timeout") => {
                self.connect_timeout = secs(mode, name, &v)?;
                self.read_timeout = self.connect_timeout;
            }
            (Mode::Wget, "connect-timeout" | "dns-timeout") => self.connect_timeout = secs(mode, name, &v)?,
            (Mode::Wget, "read-timeout") => self.read_timeout = secs(mode, name, &v)?,
            (Mode::Wget, "no-check-certificate") => self.insecure = true,
            (Mode::Wget, "user" | "http-user") => self.user = Some(v),
            (Mode::Wget, "password" | "http-password") => self.password = Some(v),
            (Mode::Wget, "max-redirect") => self.max_redirs = count(mode, name, &v)?,
            (Mode::Wget, "content-on-error") => self.errors = HttpErrors::FailWithBody,
            (Mode::Wget, "save-headers") => self.include = true,
            _ => {}
        }
        Ok(())
    }

    /// `Name: value` adds a header; `Name;` adds it empty; `Name:` (curl's "remove") adds nothing.
    fn header(&mut self, h: &str) {
        if let Some(name) = h.strip_suffix(';').filter(|n| !n.contains(':')) {
            self.headers.push((name.trim().to_string(), String::new()));
        } else if let Some((name, value)) = h.split_once(':').filter(|(_, v)| !v.trim().is_empty()) {
            self.headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    fn output(&mut self, path: &str) -> Result<(), Exit> {
        match path {
            "-" => Ok(()),
            "/dev/null" => {
                self.discard = true;
                Ok(())
            }
            _ => Err(no_files(self.mode, path)),
        }
    }

    fn has(&self, name: &str) -> bool {
        self.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    fn method(&self) -> String {
        match &self.method {
            Some(m) => m.clone(),
            None if self.head => "HEAD".into(),
            None if !self.data.is_empty() && !self.get => "POST".into(),
            None => "GET".into(),
        }
    }

    /// The request body: the data pieces joined with `&` (`--json` ones concatenated).
    fn body(&self) -> std::io::Result<Option<Vec<u8>>> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let mut body = Vec::new();
        for (i, d) in self.data.iter().enumerate() {
            if i > 0 && !self.json {
                body.push(b'&');
            }
            match d {
                Data::Raw(s) => body.extend_from_slice(s.as_bytes()),
                Data::Encode(s) => body.extend_from_slice(urlencode_arg(s).as_bytes()),
                Data::File { path, binary } => {
                    let bytes = if path == "-" {
                        let mut b = Vec::new();
                        std::io::stdin().read_to_end(&mut b)?;
                        b
                    } else {
                        std::fs::read(path)?
                    };
                    if *binary {
                        body.extend(bytes);
                    } else {
                        body.extend(bytes.into_iter().filter(|b| *b != b'\r' && *b != b'\n'));
                    }
                }
            }
        }
        Ok(Some(body))
    }

    fn client(&self) -> reqwest::Result<reqwest::Client> {
        let policy = if self.follow { Policy::limited(self.max_redirs) } else { Policy::none() };
        let mut b = reqwest::Client::builder().redirect(policy).danger_accept_invalid_certs(self.insecure);
        if let Some(d) = self.connect_timeout {
            b = b.connect_timeout(d);
        }
        if let Some(d) = self.read_timeout {
            b = b.read_timeout(d);
        }
        if let Some(d) = self.max_time {
            b = b.timeout(d);
        }
        b.build()
    }

    async fn run(&self, out: &mut impl Write, err: &mut impl Write) -> i32 {
        if self.help {
            let usage = if self.mode == Mode::Wget { WGET_USAGE } else { CURL_USAGE };
            let _ = out.write_all(usage.as_bytes());
            return 0;
        }
        if self.version {
            let _ = writeln!(out, "{} (kongming {})", self.mode.name(), env!("CARGO_PKG_VERSION"));
            return 0;
        }
        if self.urls.is_empty() {
            let _ = match self.mode {
                Mode::Curl => writeln!(err, "curl: no URL specified\ncurl: try 'curl --help'"),
                Mode::Wget => writeln!(err, "wget: missing URL\nTry 'wget --help'"),
            };
            return 2;
        }
        let body = match self.body() {
            Ok(b) => b,
            Err(e) => return self.report(err, &failure(Kind::Read, format!("can't read the data to send: {e}"))),
        };
        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return self.report(err, &failure(Kind::Other, format!("HTTP client: {e}"))),
        };
        let mut code = 0;
        for url in &self.urls {
            let start = Instant::now();
            let mut info = Info::default();
            let result = self.transfer(&client, url, body.as_deref(), out, err, &mut info).await;
            if let Some(w) = &self.write_out {
                info.time = start.elapsed();
                let _ = out.write_all(write_out(w, &info).as_bytes());
                let _ = out.flush();
            }
            if let Err(f) = result {
                code = self.report(err, &f);
            }
        }
        code
    }

    /// Print a failure (curl stays quiet with -s unless -S) and return its exit code.
    fn report(&self, err: &mut impl Write, f: &Failure) -> i32 {
        let code = f.kind.code(self.mode);
        if !f.message.is_empty() {
            let _ = match self.mode {
                Mode::Curl if self.silent && !self.show_error => Ok(()),
                Mode::Curl => writeln!(err, "curl: ({code}) {}", f.message),
                Mode::Wget => writeln!(err, "wget: {}", f.message),
            };
        }
        code
    }

    async fn transfer(
        &self,
        client: &reqwest::Client,
        raw: &str,
        body: Option<&[u8]>,
        out: &mut impl Write,
        err: &mut impl Write,
        info: &mut Info,
    ) -> Result<(), Failure> {
        let with_scheme = if raw.contains("://") { raw.to_string() } else { format!("http://{raw}") };
        let mut url = Url::parse(&with_scheme).map_err(|e| failure(Kind::Url, format!("bad URL {raw}: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(failure(Kind::Url, format!("protocol \"{}\" not supported", url.scheme())));
        }
        if let (true, Some(b)) = (self.get, body) {
            let data = String::from_utf8_lossy(b);
            let query = match url.query() {
                Some(q) if !q.is_empty() => format!("{q}&{data}"),
                _ => data.into_owned(),
            };
            url.set_query(Some(&query));
        }
        info.url = url.to_string();
        let m = self.method();
        let method = Method::from_bytes(m.as_bytes()).map_err(|_| failure(Kind::Usage, format!("bad method {m}")))?;
        let mut req = client.request(method, url.clone());
        for (name, value) in &self.headers {
            req = req.header(name.as_str(), value.as_str());
        }
        if let Some(ua) = &self.user_agent {
            req = req.header(USER_AGENT, ua.as_str());
        } else if !self.has("user-agent") {
            let ua = if self.mode == Mode::Wget { "Wget/1.21 (kongming)" } else { "curl/8.0 (kongming)" };
            req = req.header(USER_AGENT, ua);
        }
        if !self.has("accept") {
            req = req.header(ACCEPT, if self.json { "application/json" } else { "*/*" });
        }
        let send = if self.get { None } else { body };
        if let Some(b) = send {
            if !self.has("content-type") {
                let ct = if self.json { "application/json" } else { "application/x-www-form-urlencoded" };
                req = req.header(CONTENT_TYPE, ct);
            }
            req = req.body(b.to_vec());
        }
        if let Some(user) = &self.user {
            req = req.basic_auth(user, self.password.as_ref());
        }
        let request = req.build().map_err(|e| failure(Kind::Usage, format!("bad request: {e}")))?;
        if self.verbose {
            let host = url.host_str().unwrap_or("");
            let host = match url.port() {
                Some(p) => format!("{host}:{p}"),
                None => host.to_string(),
            };
            let path = &url[url::Position::BeforePath..];
            let _ = writeln!(err, "> {} {path} HTTP/1.1", request.method());
            let _ = writeln!(err, "> Host: {host}");
            for (n, v) in request.headers() {
                let _ = writeln!(err, "> {n}: {}", String::from_utf8_lossy(v.as_bytes()));
            }
            let _ = writeln!(err, ">");
        }

        let mut resp = client.execute(request).await.map_err(|e| self.network(&e, &url))?;
        let status = resp.status();
        info.code = status.as_u16();
        info.url = resp.url().to_string();
        info.content_type = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        info.remote_ip = resp.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        if status.is_redirection() {
            let location = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok());
            info.redirect_url = location.and_then(|l| resp.url().join(l).ok()).map(String::from).unwrap_or_default();
        }
        let reason = status.canonical_reason().unwrap_or("");
        let mut lines = vec![format!("{:?} {} {reason}", resp.version(), status.as_u16()).trim_end().to_string()];
        for (n, v) in resp.headers() {
            lines.push(format!("{n}: {}", String::from_utf8_lossy(v.as_bytes())));
        }
        if self.verbose {
            for l in &lines {
                let _ = writeln!(err, "< {l}");
            }
            let _ = writeln!(err, "<");
        }
        if self.server_response {
            for l in &lines {
                let _ = writeln!(err, "  {l}");
            }
        }
        let http_error = status.as_u16() >= 400 && self.errors != HttpErrors::Ignore;
        let http_failure = || match self.mode {
            Mode::Curl => failure(Kind::Http, format!("The requested URL returned error: {}", status.as_u16())),
            Mode::Wget => failure(Kind::Http, format!("server returned error: {}", lines[0])),
        };
        if http_error && self.errors == HttpErrors::Fail {
            return Err(http_failure());
        }
        if self.include || (self.mode == Mode::Curl && self.head) {
            put(out, format!("{}\n\n", lines.join("\n")).as_bytes())?;
        }
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.network(&e, &url))? {
            info.size += chunk.len() as u64;
            if !self.discard {
                put(out, &chunk)?;
            }
        }
        if http_error {
            return Err(http_failure());
        }
        Ok(())
    }

    /// Classify a reqwest error by its kind and its sources (best effort: the
    /// resolver and rustls errors only show up as text).
    fn network(&self, e: &reqwest::Error, url: &Url) -> Failure {
        let host = url.host_str().unwrap_or("");
        let mut causes = String::new();
        let mut source = std::error::Error::source(e);
        while let Some(s) = source {
            causes.push_str(": ");
            causes.push_str(&s.to_string());
            source = s.source();
        }
        let detail = format!("{e}{causes}");
        let lower = causes.to_lowercase();
        if e.is_timeout() || lower.contains("timed out") {
            failure(Kind::Timeout, format!("Operation timed out: {detail}"))
        } else if e.is_redirect() {
            failure(Kind::Redirects, format!("Maximum ({}) redirects followed", self.max_redirs))
        } else if lower.contains("dns error") || lower.contains("failed to lookup address") {
            failure(Kind::Dns, format!("Could not resolve host: {host}"))
        } else if ["certificate", "tls", "handshake", "alert"].iter().any(|w| lower.contains(*w)) {
            failure(Kind::Tls, format!("TLS connect error: {detail}"))
        } else if e.is_connect() {
            failure(Kind::Connect, format!("Failed to connect to {host}: {detail}"))
        } else {
            failure(Kind::Other, format!("Failure when receiving data from the peer: {detail}"))
        }
    }
}

/// Write and flush.
fn put(out: &mut impl Write, bytes: &[u8]) -> Result<(), Failure> {
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        // A closed pipe (`| head`) ends the transfer quietly.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(failure(Kind::Write, String::new())),
        Err(e) => Err(failure(Kind::Write, format!("Failure writing output to destination: {e}"))),
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Usage,
    Url,
    Dns,
    Connect,
    Timeout,
    Tls,
    Redirects,
    Http,
    Write,
    Read,
    Other,
}

impl Kind {
    fn code(self, mode: Mode) -> i32 {
        match mode {
            Mode::Curl => match self {
                Kind::Usage => 2,
                Kind::Url => 3,
                Kind::Dns => 6,
                Kind::Connect => 7,
                Kind::Http => 22,
                Kind::Write => 23,
                Kind::Read => 26,
                Kind::Timeout => 28,
                Kind::Tls => 35,
                Kind::Redirects => 47,
                Kind::Other => 56,
            },
            // GNU wget: 1 generic, 2 usage, 3 file I/O, 4 network, 5 TLS, 8 server error.
            Mode::Wget => match self {
                Kind::Url | Kind::Read => 1,
                Kind::Usage => 2,
                Kind::Write => 3,
                Kind::Dns | Kind::Connect | Kind::Timeout | Kind::Other => 4,
                Kind::Tls => 5,
                Kind::Http | Kind::Redirects => 8,
            },
        }
    }
}

struct Failure {
    kind: Kind,
    /// Empty: exit without a message.
    message: String,
}

fn failure(kind: Kind, message: String) -> Failure {
    Failure { kind, message }
}

/// What `-w` can print about a transfer.
#[derive(Default)]
struct Info {
    code: u16,
    content_type: String,
    url: String,
    size: u64,
    time: Duration,
    redirect_url: String,
    remote_ip: String,
}

/// Expand curl's `-w` format: `%{variable}`, `%%` and `\n`-style escapes.
fn write_out(format: &str, info: &Info) -> String {
    const ESCAPES: [(&str, &str); 5] = [("%%", "%"), ("\\n", "\n"), ("\\r", "\r"), ("\\t", "\t"), ("\\\\", "\\")];
    let mut s = String::new();
    let mut rest = format;
    while let Some(c) = rest.chars().next() {
        if let Some((var, after)) = rest.strip_prefix("%{").and_then(|r| r.split_once('}')) {
            let value = match var {
                "http_code" | "response_code" => format!("{:03}", info.code),
                "content_type" => info.content_type.clone(),
                "url_effective" | "url" => info.url.clone(),
                "size_download" => info.size.to_string(),
                "time_total" => format!("{:.6}", info.time.as_secs_f64()),
                "redirect_url" => info.redirect_url.clone(),
                "remote_ip" => info.remote_ip.clone(),
                _ => format!("%{{{var}}}"),
            };
            s.push_str(&value);
            rest = after;
        } else if let Some((from, to)) = ESCAPES.iter().find(|(from, _)| rest.starts_with(*from)) {
            s.push_str(to);
            rest = &rest[from.len()..];
        } else {
            s.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    s
}

/// `--data-urlencode`: `name=value` encodes the value, `=value` and `value` encode all of it.
fn urlencode_arg(arg: &str) -> String {
    match arg.split_once('=') {
        Some(("", value)) => percent(value),
        Some((name, value)) => format!("{name}={}", percent(value)),
        None => percent(arg),
    }
}

/// Percent-encode everything but RFC 3986 unreserved characters, as curl does.
fn percent(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 15)] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn curl(a: &[&str]) -> Result<Opts, Exit> {
        parse(Mode::Curl, args(a))
    }

    fn wget(a: &[&str]) -> Result<Opts, Exit> {
        parse(Mode::Wget, args(a))
    }

    fn body(o: &Opts) -> String {
        String::from_utf8(o.body().unwrap().unwrap()).unwrap()
    }

    #[test]
    fn short_flags_combine_and_take_attached_values() {
        let a = ["-sSL", "-XPUT", "-HX-A: b", "-m2.5", "--max-redirs=3", "--user-agent", "ua", "example.com"];
        let o = curl(&a).unwrap();
        assert!(o.silent && o.show_error && o.follow);
        assert_eq!(o.method.as_deref(), Some("PUT"));
        assert_eq!(o.headers, vec![("X-A".to_string(), "b".to_string())]);
        assert_eq!(o.max_time, Some(Duration::from_millis(2500)));
        assert_eq!(o.max_redirs, 3);
        assert_eq!(o.user_agent.as_deref(), Some("ua"));
        assert_eq!(o.urls, vec!["example.com"]);

        let o = curl(&["-fsSo/dev/null", "-w", "%{http_code}", "u"]).unwrap();
        assert!(o.discard && o.silent && o.errors == HttpErrors::Fail);
        assert_eq!(o.write_out.as_deref(), Some("%{http_code}"));
        assert!(!curl(&["-sI", "u"]).unwrap().follow);
        assert_eq!(curl(&["-sI", "u"]).unwrap().method(), "HEAD");
    }

    #[test]
    fn data_is_joined_and_sets_the_method() {
        let a = ["-d", "a=1", "--data-urlencode", "q=a b&c", "--data-raw", "@x", "--data-urlencode", "é/"];
        let o = curl(&a).unwrap();
        assert_eq!(body(&o), "a=1&q=a%20b%26c&@x&%C3%A9%2F");
        assert_eq!(o.method(), "POST");
        assert_eq!(curl(&["-G", "-d", "a=1", "u"]).unwrap().method(), "GET");
        let o = curl(&["--json", r#"{"k":1}"#, "u"]).unwrap();
        assert!(o.json);
        assert_eq!(body(&o), r#"{"k":1}"#);
        assert_eq!(curl(&["u"]).unwrap().body().unwrap(), None);
        let o = curl(&["-u", "me:pw:x", "-b", "k=v", "-e", "https://ref/;auto", "u"]).unwrap();
        assert_eq!((o.user.as_deref(), o.password.as_deref()), (Some("me"), Some("pw:x")));
        assert!(o.has("cookie") && o.has("REFERER"));
    }

    #[test]
    fn files_are_refused_and_unknown_options_rejected() {
        let e = curl(&["-o", "page.html", "u"]).unwrap_err();
        assert_eq!(e.code, 23);
        assert!(e.message.contains("stdout"), "{}", e.message);
        assert_eq!(curl(&["-O", "u"]).unwrap_err().code, 23);
        assert!(curl(&["-o", "-", "u"]).is_ok());
        let e = curl(&["-sz", "u"]).unwrap_err();
        assert_eq!(e.code, 2);
        assert!(e.message.contains("-z"), "{}", e.message);
        assert_eq!(curl(&["--nope", "u"]).unwrap_err().code, 2);
        assert_eq!(curl(&["-H"]).unwrap_err().code, 2);
        assert!(curl(&["--compressed", "-#", "--retry", "3", "--http1.1", "u"]).is_ok());

        assert_eq!(wget(&["-O", "/tmp/x", "u"]).unwrap_err().code, 1);
        assert_eq!(wget(&["-qO/tmp/x", "u"]).unwrap_err().code, 1);
        let ok: [&[&str]; 4] = [&["-qO-", "u"], &["-qO", "-", "u"], &["-SO-", "u"], &["-nv", "-O", "/dev/null", "u"]];
        for a in ok {
            assert!(wget(a).is_ok(), "{a:?}");
        }
    }

    #[test]
    fn wget_defaults_and_options() {
        let o = wget(&["u"]).unwrap();
        assert!(o.follow && o.errors == HttpErrors::Fail);
        assert_eq!(o.max_redirs, 20);
        let o = wget(&["-q", "--spider", "-S", "--header=X: 1", "--max-redirect", "0", "-T5", "u"]).unwrap();
        assert!(o.silent && o.head && o.server_response);
        assert_eq!(o.method(), "HEAD");
        assert_eq!(o.max_redirs, 0);
        assert_eq!(o.read_timeout, Some(Duration::from_secs(5)));
        let o = wget(&["--post-data=a=1", "u"]).unwrap();
        assert_eq!((o.method(), body(&o)), ("POST".to_string(), "a=1".to_string()));
        assert_eq!(wget(&["--method=put", "u"]).unwrap().method(), "PUT");
    }

    #[test]
    fn write_out_expands_variables_and_escapes() {
        let info = Info { code: 404, url: "http://x/".into(), size: 12, ..Info::default() };
        let format = r"%{http_code} %{url_effective} %{size_download}%% %{nope}\n";
        assert_eq!(write_out(format, &info), "404 http://x/ 12% %{nope}\n");
        assert_eq!(write_out("%{response_code}", &Info::default()), "000");
    }
}
