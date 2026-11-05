//! Current ChatGPT direct-token OAuth. Legacy Codex credentials are never sent
//! to the Platform resource; a successful new login is required to migrate.
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const FLOW: &str = "chatgpt-direct";
const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const MIGRATE: &str = "run ahi login to migrate to the current OpenAI flow";
const EXPIRED: &str = "OpenAI authentication expired or rejected: run ahi login";
const CATALOG_FORBIDDEN: &str = "OpenAI catalog permission denied (HTTP 403): this grant may not support model discovery; curated models and the last-good catalog are unchanged";

#[derive(Deserialize, Serialize)]
struct Credential {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    // Unix seconds, including Pi's three-minute safety margin.
    expires_at: i64,
    client_id: String,
    resource: String,
    scopes: Vec<String>,
    flow: String,
}

fn valid_secret(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

pub(super) fn catalog_source(base_url: &str) -> Result<String> {
    if base_url.trim_end_matches('/') != OPENAI_ORIGIN {
        return Err(Error::new("OpenAI browser credentials require baseUrl https://api.openai.com/v1; remove the conflicting OpenAI baseUrl (use a separate gateway id for proxies)"));
    }
    Ok(format!("{OPENAI_ORIGIN}/models"))
}

pub(super) fn refresh(state: &Path, base_url: &str) -> Result<Vec<String>> {
    let source = catalog_source(base_url)?;
    refresh_at(state, &source, OPENAI_TOKEN)
}

fn refresh_at(state: &Path, catalog_url: &str, token_url: &str) -> Result<Vec<String>> {
    authenticated(state, token_url, |token| fetch_models(catalog_url, token))
}

/// The only destination a browser credential is ever sent to for inference.
pub(super) const INFERENCE_URL: &str = "https://api.openai.com/v1/responses";

pub(crate) fn inference_response(
    state: &Path,
    body: &[u8],
    client: &ureq::Agent,
) -> Result<ureq::Response> {
    inference_response_at(state, body, INFERENCE_URL, OPENAI_TOKEN, client)
}

#[cfg(test)]
pub(crate) fn fixture_inference_response(
    state: &Path,
    body: &[u8],
    inference_url: &str,
    token_url: &str,
) -> Result<ureq::Response> {
    let client = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(120))
        .build();
    inference_response_at(state, body, inference_url, token_url, &client)
}

fn inference_response_at(
    state: &Path,
    body: &[u8],
    inference_url: &str,
    token_url: &str,
    client: &ureq::Agent,
) -> Result<ureq::Response> {
    authenticated(state, token_url, |token| {
        match client
            .post(inference_url)
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream")
            .set("Authorization", &format!("Bearer {token}"))
            .send_bytes(body)
        {
            Ok(response) if response.status() == 200 => Ok(response),
            Ok(response) => Err(FetchError::Other(status_error(
                "inference",
                response.status(),
            ))),
            Err(ureq::Error::Status(401, _)) => Err(FetchError::Unauthorized),
            Err(ureq::Error::Status(status, _)) => {
                Err(FetchError::Other(status_error("inference", status)))
            }
            Err(_) => Err(FetchError::Other(Error::transport(
                "OpenAI inference network, TLS, or timeout error; generation was not retried",
            ))),
        }
    })
}

fn authenticated<T>(
    state: &Path,
    token_url: &str,
    mut operation: impl FnMut(&str) -> std::result::Result<T, FetchError>,
) -> Result<T> {
    let mut credential = read_credential(state)?;
    let mut renewed = false;
    if credential.expires_at <= now() {
        credential = renew_token(token_url, &credential)?;
        write_credential(state, &credential)?;
        renewed = true;
    }
    match operation(&credential.access_token) {
        Ok(models) => Ok(models),
        Err(FetchError::Unauthorized) if !renewed => {
            credential = renew_token(token_url, &credential)?;
            write_credential(state, &credential)?;
            operation(&credential.access_token).map_err(FetchError::into_error)
        }
        Err(error) => Err(error.into_error()),
    }
}

fn read_credential(state: &Path) -> Result<Credential> {
    let file = fs::File::open(state.join("credentials/openai.json"))
        .map_err(|_| Error::auth("OpenAI credential unavailable: run ahi login"))?;
    let bytes = bounded_read(file)?;
    // Never propagate serde errors: they can contain secret values.
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::auth("invalid OpenAI credential: run ahi login"))?;
    if value.get("flow").and_then(Value::as_str) != Some(FLOW) {
        return Err(Error::auth(MIGRATE));
    }
    let credential: Credential = serde_json::from_value(value)
        .map_err(|_| Error::auth("invalid OpenAI credential: run ahi login"))?;
    if !valid_secret(&credential.access_token)
        || !valid_secret(&credential.refresh_token)
        || !valid_secret(&credential.client_id)
        || credential.resource != OPENAI_ORIGIN
        || !credential.scopes.iter().any(|scope| scope == DIRECT_SCOPE)
    {
        return Err(Error::auth(
            "invalid OpenAI direct-token credential: run ahi login",
        ));
    }
    Ok(credential)
}

enum FetchError {
    Unauthorized,
    Other(Error),
}

impl FetchError {
    fn into_error(self) -> Error {
        match self {
            Self::Unauthorized => Error::auth(EXPIRED),
            Self::Other(error) => error,
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(FETCH_TIMEOUT)
        .build()
}

fn fetch_models(url: &str, token: &str) -> std::result::Result<Vec<String>, FetchError> {
    let response = match agent()
        .get(url)
        .set("Accept", "application/json")
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(401, _)) => return Err(FetchError::Unauthorized),
        Err(ureq::Error::Status(403, _)) => {
            return Err(FetchError::Other(Error::new(CATALOG_FORBIDDEN)))
        }
        Err(ureq::Error::Status(status, _)) => {
            return Err(FetchError::Other(status_error("catalog", status)))
        }
        Err(_) => {
            return Err(FetchError::Other(Error::new(
                "OpenAI catalog network or TLS error",
            )))
        }
    };
    if !(200..300).contains(&response.status()) {
        return Err(FetchError::Other(status_error(
            "catalog",
            response.status(),
        )));
    }
    let bytes = bounded_read(response.into_reader()).map_err(FetchError::Other)?;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Response {
        Platform { data: Vec<PlatformModel> },
        ChatGpt { models: Vec<ChatGptModel> },
    }
    #[derive(Deserialize)]
    struct PlatformModel {
        id: String,
    }
    #[derive(Deserialize)]
    struct ChatGptModel {
        slug: String,
    }
    let catalog: Response = serde_json::from_slice(&bytes)
        .map_err(|_| FetchError::Other(Error::new("invalid OpenAI model catalog")))?;
    let models: Vec<String> = match catalog {
        Response::Platform { data } => data.into_iter().map(|model| model.id).collect(),
        Response::ChatGpt { models } => models.into_iter().map(|model| model.slug).collect(),
    };
    if models.iter().any(|id| !valid_model_id(id)) {
        return Err(FetchError::Other(Error::new("invalid OpenAI model id")));
    }
    Ok(models)
}

fn status_error(operation: &str, status: u16) -> Error {
    if (300..400).contains(&status) {
        Error::transport(format!(
            "OpenAI {operation} redirects are not followed (HTTP {status})"
        ))
    } else if status == 401 || status == 403 {
        Error::auth(format!(
            "OpenAI {operation} HTTP {status}: run ahi login if authorization was revoked"
        ))
    } else {
        Error::transport(format!(
            "OpenAI {operation} HTTP {status}: run ahi login if authorization was revoked"
        ))
    }
}

fn bounded_read(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(BODY_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("cannot read OpenAI response or credential"))?;
    if bytes.len() as u64 > BODY_CAP {
        return Err(Error::new(
            "OpenAI response or credential is larger than 1 MiB",
        ));
    }
    Ok(bytes)
}

fn request_token(url: &str, fields: &[(&str, &str)]) -> Result<Value> {
    let body = fields
        .iter()
        .map(|(key, value)| format!("{key}={}", form_escape(value)))
        .collect::<Vec<_>>()
        .join("&");
    let response = agent()
        .post(url)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&body)
        .map_err(|error| match error {
            ureq::Error::Status(status, _) => status_error("token request", status),
            _ => Error::new("OpenAI token request network or TLS error"),
        })?;
    if !(200..300).contains(&response.status()) {
        return Err(status_error("token request", response.status()));
    }
    serde_json::from_slice(&bounded_read(response.into_reader())?)
        .map_err(|_| Error::new("invalid OpenAI token response"))
}

fn credential_from_token(
    token: &Value,
    client_id: &str,
    previous_refresh: Option<&str>,
) -> Result<Credential> {
    let invalid = || {
        Error::new("invalid OpenAI token response: expected tokens, positive expires_in and chatgpt.tokens.use.direct scope")
    };
    let access = token
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|v| valid_secret(v))
        .ok_or_else(invalid)?;
    let refresh = token
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|v| valid_secret(v))
        .or(previous_refresh)
        .ok_or_else(invalid)?;
    let expires_in = token
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or_else(invalid)?;
    let scopes: Vec<String> = token
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if !scopes.iter().any(|scope| scope == DIRECT_SCOPE) || !valid_secret(client_id) {
        return Err(invalid());
    }
    let expires_at = i64::try_from(expires_in)
        .ok()
        .and_then(|ttl| now().checked_add(ttl))
        .and_then(|expiry| expiry.checked_sub(180))
        .ok_or_else(invalid)?;
    Ok(Credential {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in,
        expires_at,
        client_id: client_id.to_string(),
        resource: OPENAI_ORIGIN.to_string(),
        scopes,
        flow: FLOW.to_string(),
    })
}

fn renew_token(url: &str, credential: &Credential) -> Result<Credential> {
    let token = request_token(
        url,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &credential.client_id),
            ("refresh_token", &credential.refresh_token),
            ("resource", OPENAI_ORIGIN),
        ],
    )?;
    credential_from_token(
        &token,
        &credential.client_id,
        Some(&credential.refresh_token),
    )
}

fn write_credential(state: &Path, credential: &Credential) -> Result<()> {
    let dir = state.join("credentials");
    fs::create_dir_all(&dir)
        .map_err(|_| Error::new("cannot create OpenAI credential directory"))?;
    let path = dir.join("openai.json");
    let temporary = dir.join("openai.json.tmp");
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| Error::new("cannot create temporary OpenAI credential"))?;
    let result = (|| {
        let bytes = serde_json::to_vec(credential)
            .map_err(|_| Error::new("cannot encode OpenAI credential"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| Error::new("cannot write OpenAI credential"))?;
        fs::rename(&temporary, &path).map_err(|_| Error::new("cannot replace OpenAI credential"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn device_id(state: &Path) -> Result<String> {
    fs::create_dir_all(state).map_err(|_| Error::new("cannot create OpenAI state directory"))?;
    let path = state.join("openai-device-id");
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let mut bytes = [0_u8; 16];
            getrandom::getrandom(&mut bytes)
                .map_err(|_| Error::new("cannot generate OpenAI device id"))?;
            bytes[6] = (bytes[6] & 15) | 64;
            bytes[8] = (bytes[8] & 63) | 128;
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let uuid = format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            );
            file.write_all(uuid.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|_| Error::new("cannot write OpenAI device id"))?;
            Ok(uuid)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file =
                fs::File::open(path).map_err(|_| Error::new("cannot read OpenAI device id"))?;
            let mut value = String::new();
            file.take(128)
                .read_to_string(&mut value)
                .map_err(|_| Error::new("invalid OpenAI device id"))?;
            let value = value.trim();
            if value.len() != 36
                || !value.bytes().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == b'-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
            {
                return Err(Error::new(
                    "invalid existing openai-device-id; restore a valid installation UUID",
                ));
            }
            Ok(value.to_ascii_lowercase())
        }
        Err(_) => Err(Error::new("cannot create OpenAI device id")),
    }
}

fn authorization_url(verifier: &str, state: &str, nonce: &str, device: &str) -> String {
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let host = format!("urn:uuid:{device}");
    let fields = [
        ("client_id", OPENAI_CLIENT_ID),
        ("agent_name_hint", "ahi"),
        ("ext_agent_host_id", &host),
        ("response_type", "code"),
        ("redirect_uri", OPENAI_REDIRECT),
        ("resource", OPENAI_ORIGIN),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("nonce", nonce),
    ];
    format!(
        "{OPENAI_AUTHORIZE}?{}",
        fields
            .iter()
            .map(|(key, value)| format!("{key}={}", form_escape(value)))
            .collect::<Vec<_>>()
            .join("&")
    )
}

struct Authorization {
    code: String,
    client_id: String,
}

fn authorization_from_callback(text: &str, expected_state: &str) -> Result<Authorization> {
    let text = text.trim();
    let query = text
        .strip_prefix(&format!("{OPENAI_REDIRECT}?"))
        .or_else(|| text.strip_prefix("/auth/callback?"))
        .ok_or_else(|| {
            Error::new(
                "paste the full OpenAI callback URL from http://127.0.0.1:1455/auth/callback",
            )
        })?;
    if query.contains('#') {
        return Err(Error::new("invalid OpenAI callback URL"));
    }
    let mut fields = BTreeMap::new();
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| Error::new("invalid OpenAI callback query"))?;
        let key = form_unescape(key);
        if fields.insert(key, form_unescape(value)).is_some() {
            return Err(Error::new("duplicate OpenAI callback parameter"));
        }
    }
    if fields.get("state").map(String::as_str) != Some(expected_state) {
        return Err(Error::new("OpenAI callback state missing or mismatched"));
    }
    if fields.contains_key("error") {
        return Err(Error::new(
            "OpenAI authorization denied in browser; run ahi login to retry",
        ));
    }
    let code = fields
        .remove("code")
        .filter(|v| valid_secret(v))
        .ok_or_else(|| Error::new("OpenAI callback is missing code"))?;
    let client_id = fields
        .remove("client_id")
        .filter(|v| valid_secret(v))
        .ok_or_else(|| Error::new("OpenAI registration callback is missing issued client_id"))?;
    Ok(Authorization { code, client_id })
}

fn authorization_from_paste(text: &str, expected_state: &str) -> Result<Authorization> {
    if !text.trim().starts_with(&format!("{OPENAI_REDIRECT}?")) {
        return Err(Error::new(
            "paste the full OpenAI callback URL, not a bare code",
        ));
    }
    authorization_from_callback(text, expected_state)
}

fn bind_callback(port: u16) -> Result<TcpListener> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|_| Error::new(format!("OpenAI callback port {port} unavailable; cancel other pending logins or the Codex CLI and retry")))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| Error::new("cannot prepare OpenAI callback listener"))?;
    Ok(listener)
}

fn receive_callback(
    listener: TcpListener,
    state: &str,
    cancelled: &AtomicBool,
) -> Result<Authorization> {
    // Browsers can preconnect without sending a request. Poll bounded,
    // nonblocking connections so a spare socket cannot block the real redirect.
    // Dropping this collection also closes every spare connection on completion.
    let mut connections: Vec<(std::net::TcpStream, Vec<u8>, std::time::Instant)> = Vec::new();
    while !cancelled.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if connections.len() < 16 {
                    stream
                        .set_nonblocking(true)
                        .map_err(|_| Error::new("cannot prepare OpenAI callback connection"))?;
                    connections.push((stream, Vec::new(), std::time::Instant::now()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(Error::new("OpenAI callback listener failed")),
        }
        let mut index = 0;
        while index < connections.len() {
            let (stream, bytes, started) = &mut connections[index];
            if started.elapsed() >= FETCH_TIMEOUT {
                connections.swap_remove(index);
                continue;
            }
            let mut chunk = [0; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => {
                    connections.swap_remove(index);
                    continue;
                }
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    index += 1;
                    continue;
                }
                Err(_) => {
                    connections.swap_remove(index);
                    continue;
                }
            }
            let line_end = bytes.windows(2).position(|pair| pair == b"\r\n");
            if line_end.is_none() && bytes.len() < 8192 {
                index += 1;
                continue;
            }
            let line = line_end.and_then(|end| std::str::from_utf8(&bytes[..end]).ok());
            let path = line
                .and_then(|line| line.strip_prefix("GET "))
                .and_then(|rest| rest.split_whitespace().next());
            if path.is_some_and(|path| !path.starts_with("/auth/callback?")) {
                reply_callback(stream, "404 Not Found", "Not an OpenAI callback.\n");
                connections.swap_remove(index);
                continue;
            }
            let result = path
                .ok_or_else(|| Error::new("invalid OpenAI callback request"))
                .and_then(|path| authorization_from_callback(path, state));
            if result.is_ok() {
                reply_callback(
                    stream,
                    "200 OK",
                    "ahi login received the callback. You can close this tab.\n",
                );
            } else {
                reply_callback(
                    stream,
                    "400 Bad Request",
                    "ahi rejected the callback. Check the terminal for details.\n",
                );
            }
            return result;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(Error::new("OpenAI login cancelled"))
}

fn reply_callback(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\n{body}", body.len());
}

pub(crate) struct OpenAiLogin {
    state_dir: PathBuf,
    url: String,
    oauth_state: String,
    verifier: String,
    cancelled: Arc<AtomicBool>,
    callback: Option<std::thread::JoinHandle<()>>,
    sender: std::sync::mpsc::Sender<Result<Authorization>>,
    receiver: std::sync::mpsc::Receiver<Result<Authorization>>,
}

impl OpenAiLogin {
    pub(crate) fn start(state: &Path) -> Result<Self> {
        // Bind before exposing the URL so another session cannot receive this callback.
        let listener = bind_callback(1455)?;
        let device = device_id(state)?;
        let verifier = random_token(32)?;
        let oauth_state = random_token(32)?;
        let nonce = random_token(32)?;
        let url = authorization_url(&verifier, &oauth_state, &nonce, &device);
        let cancelled = Arc::new(AtomicBool::new(false));
        let callback_cancelled = cancelled.clone();
        let callback_state = oauth_state.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let callback_sender = sender.clone();
        let callback = std::thread::spawn(move || {
            let _ = callback_sender.send(receive_callback(
                listener,
                &callback_state,
                &callback_cancelled,
            ));
        });
        Ok(Self {
            state_dir: state.to_path_buf(),
            url,
            oauth_state,
            verifier,
            cancelled,
            callback: Some(callback),
            sender,
            receiver,
        })
    }

    pub(crate) fn authorization_url(&self) -> &str {
        &self.url
    }

    pub(crate) fn open_browser(&self) {
        try_open_browser(&self.url);
    }

    pub(crate) fn submit_callback(&self, text: &str) -> Result<()> {
        let authorization = authorization_from_paste(text, &self.oauth_state)?;
        self.sender
            .send(Ok(authorization))
            .map_err(|_| Error::new("OpenAI login ended before callback"))
    }

    pub(crate) fn poll(&mut self) -> Option<Result<()>> {
        match self.receiver.try_recv() {
            Ok(authorization) => Some(self.complete(authorization)),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(Error::new("OpenAI login ended before callback")))
            }
        }
    }

    fn stop_callback(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(callback) = self.callback.take() {
            let _ = callback.join();
        }
    }

    fn complete(&mut self, authorization: Result<Authorization>) -> Result<()> {
        self.stop_callback();
        let credential = exchange_code(OPENAI_TOKEN, &authorization?, &self.verifier)?;
        write_credential(&self.state_dir, &credential)
    }
}

impl Drop for OpenAiLogin {
    fn drop(&mut self) {
        self.stop_callback();
    }
}

pub(super) fn login(state: &Path) -> Result<()> {
    let mut login = OpenAiLogin::start(state)?;
    eprintln!("{}", login.authorization_url());
    login.open_browser();
    eprintln!("Complete browser login, or paste the full final callback URL and press enter.");
    let sender = login.sender.clone();
    let oauth_state = login.oauth_state.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            // A closed stdin (or an empty line) must not cancel browser login.
            Ok(_) if line.trim().is_empty() => {}
            Ok(_) => {
                let _ = sender.send(authorization_from_paste(&line, &oauth_state));
            }
            Err(_) => {
                let _ = sender.send(Err(Error::new("cannot read OpenAI login paste")));
            }
        }
    });
    let authorization = login
        .receiver
        .recv()
        .map_err(|_| Error::new("OpenAI login ended before callback"))?;
    login.complete(authorization)
}

fn exchange_code(url: &str, authorization: &Authorization, verifier: &str) -> Result<Credential> {
    let token = request_token(
        url,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &authorization.client_id),
            ("code", &authorization.code),
            ("code_verifier", verifier),
            ("redirect_uri", OPENAI_REDIRECT),
            ("resource", OPENAI_ORIGIN),
        ],
    )?;
    if !token
        .get("id_token")
        .and_then(Value::as_str)
        .is_some_and(valid_secret)
    {
        return Err(Error::new("OpenAI token response is missing id_token"));
    }
    // Like Pi, do not decode the ID token for account/profile data. The direct
    // flow uses bearer auth, not the legacy Codex ChatGPT-Account-ID header.
    credential_from_token(&token, &authorization.client_id, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = r#"{"access_token":"new-secret","refresh_token":"rotated-secret","expires_in":3600,"scope":"openid chatgpt.tokens.use.direct","id_token":"fixture-id-token"}"#;

    fn fixture_dir() -> PathBuf {
        crate::fixture_directory::create_unique_fixture_directory("openai-direct")
    }

    fn credential_dir() -> PathBuf {
        let dir = fixture_dir();
        let credential = credential_from_token(
            &serde_json::json!({
                "access_token":"fixture-secret", "refresh_token":"refresh-secret",
                "expires_in":3600, "scope":DIRECT_SCOPE
            }),
            "issued-fixture-client",
            None,
        )
        .unwrap();
        write_credential(&dir, &credential).unwrap();
        dir
    }

    fn server(
        replies: Vec<(&'static str, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                let len: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|len| len.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                let mut body_bytes = vec![0; len];
                stream.read_exact(&mut body_bytes).unwrap();
                requests.push(format!(
                    "{headers}{}",
                    String::from_utf8(body_bytes).unwrap()
                ));
                // Write errors are expected when the client enforces its cap.
                let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nLocation: http://127.0.0.1:1/stolen\r\n\r\n{body}", body.len());
            }
            requests
        });
        (url, handle)
    }

    #[test]
    fn direct_authorization_requests_dynamic_registration_resource_nonce_and_pkce() {
        let url = authorization_url(
            "verifier-value",
            "state-value",
            "nonce-value",
            "00112233-4455-4677-8899-aabbccddeeff",
        );
        assert!(url.starts_with("https://auth.openai.com/api/accounts/authorize?"));
        for parameter in [
            "client_id=dynamic_agent_client",
            "agent_name_hint=ahi",
            "ext_agent_host_id=urn%3Auuid%3A00112233-4455-4677-8899-aabbccddeeff",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback",
            "resource=https%3A%2F%2Fapi.openai.com%2Fv1",
            "chatgpt.tokens.use.direct",
            "code_challenge_method=S256",
            "state=state-value",
            "nonce=nonce-value",
        ] {
            assert!(url.contains(parameter), "missing {parameter}");
        }
        assert!(!url.contains("verifier-value"));
        assert!(!url.contains("codex"));
    }

    #[test]
    fn device_id_is_stable_uuid_and_invalid_state_is_not_overwritten() {
        let dir = fixture_dir();
        let id = device_id(&dir).unwrap();
        assert_eq!(device_id(&dir).unwrap(), id);
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        fs::write(dir.join("openai-device-id"), "malformed fixture").unwrap();
        assert!(device_id(&dir).is_err());
        assert_eq!(
            fs::read_to_string(dir.join("openai-device-id")).unwrap(),
            "malformed fixture"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn callback_requires_state_and_issued_client_and_validates_origin_and_errors() {
        let valid = format!("{OPENAI_REDIRECT}?code=abc&state=ok&client_id=issued-client");
        let authorization = authorization_from_callback(&valid, "ok").unwrap();
        assert_eq!(authorization.code, "abc");
        assert_eq!(authorization.client_id, "issued-client");
        for query in [
            "code=abc&client_id=issued-client",
            "code=abc&state=wrong&client_id=issued-client",
            "code=abc&state=ok",
            "code=&state=ok&client_id=issued-client",
            "code=abc&state=ok&state=ok&client_id=issued-client",
            "state=ok&error=fixture-secret",
        ] {
            let error = authorization_from_callback(&format!("{OPENAI_REDIRECT}?{query}"), "ok")
                .err()
                .unwrap();
            assert!(!error.to_string().contains("fixture-secret"));
        }
        for text in [
            "abc",
            "code=abc&state=ok&client_id=issued-client",
            "https://evil.invalid/auth/callback?code=abc&state=ok&client_id=issued-client",
            "http://127.0.0.1:1455/wrong?code=abc&state=ok&client_id=issued-client",
        ] {
            assert!(authorization_from_callback(text, "ok").is_err());
        }
    }

    #[test]
    fn callback_is_ready_before_browser_and_reports_port_conflict() {
        let listener = bind_callback(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(bind_callback(port)
            .err()
            .unwrap()
            .to_string()
            .contains("unavailable"));
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let handle =
            std::thread::spawn(move || receive_callback(listener, "ok", &AtomicBool::new(false)));
        client.write_all(b"GET /auth/callback?code=abc&state=ok&client_id=issued-client HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        assert_eq!(handle.join().unwrap().unwrap().code, "abc");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = String::new();
        // Read to EOF to verify the accepted connection closed. Rebinding this
        // port is not a closure check: TCP may retain it in TIME_WAIT.
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("ahi login received the callback."));
    }

    #[test]
    fn callback_handles_preconnected_spares_fragmented_requests_and_unrelated_paths() {
        let listener = bind_callback(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut spare = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        spare
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let handle =
            std::thread::spawn(move || receive_callback(listener, "ok", &AtomicBool::new(false)));
        let mut unrelated = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        unrelated
            .write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"GET /auth/callback?code=abc&state=")
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        client
            .write_all(b"ok&client_id=issued-client HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(handle.join().unwrap().unwrap().code, "abc");
        // Completion closes the old listener and every spare connection.
        assert_eq!(spare.read(&mut [0; 1]).unwrap(), 0);
    }

    #[test]
    fn callback_rejects_state_mismatch_with_http_error_and_honors_cancellation() {
        let listener = bind_callback(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let handle =
            std::thread::spawn(move || receive_callback(listener, "ok", &AtomicBool::new(false)));
        client
            .write_all(
                b"GET /auth/callback?code=abc&state=wrong&client_id=issued-client HTTP/1.1\r\n\r\n",
            )
            .unwrap();
        assert!(handle
            .join()
            .unwrap()
            .err()
            .unwrap()
            .to_string()
            .contains("state"));
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        let listener = bind_callback(0).unwrap();
        assert_eq!(
            receive_callback(listener, "ok", &AtomicBool::new(true))
                .err()
                .unwrap()
                .to_string(),
            "OpenAI login cancelled"
        );
    }

    #[test]
    fn exchange_uses_issued_client_resource_and_stores_metadata_without_id_token_or_account() {
        let dir = fixture_dir();
        let (url, requests) = server(vec![("200 OK", TOKEN.to_string())]);
        let credential = exchange_code(
            &url,
            &Authorization {
                code: "fixture-code".into(),
                client_id: "issued-fixture-client".into(),
            },
            "fixture-verifier",
        )
        .unwrap();
        write_credential(&dir, &credential).unwrap();
        let requests = requests.join().unwrap();
        for field in [
            "grant_type=authorization_code",
            "client_id=issued-fixture-client",
            "code_verifier=fixture-verifier",
            "resource=https%3A%2F%2Fapi.openai.com%2Fv1",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback",
        ] {
            assert!(requests[0].contains(field));
        }
        let stored = fs::read_to_string(dir.join("credentials/openai.json")).unwrap();
        assert!(!stored.contains("id_token"));
        assert!(!stored.contains("account"));
        let credential = read_credential(&dir).unwrap();
        assert_eq!(credential.flow, FLOW);
        assert_eq!(credential.client_id, "issued-fixture-client");
        assert_eq!(credential.expires_in, 3600);
        assert!((now() + 3418..=now() + 3420).contains(&credential.expires_at));
        assert_eq!(credential.refresh_token, "rotated-secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.join("credentials/openai.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(!render_text(&load_registry(&dir).unwrap()).contains("secret"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_credentials_require_new_login_and_failed_exchange_preserves_bytes() {
        let dir = fixture_dir();
        fs::create_dir_all(dir.join("credentials")).unwrap();
        let legacy =
            r#"{"access_token":"old-secret","refresh_token":"old-refresh","expires_in":3600}"#;
        fs::write(dir.join("credentials/openai.json"), legacy).unwrap();
        assert_eq!(
            refresh_at(&dir, "http://127.0.0.1:1", "http://127.0.0.1:1")
                .unwrap_err()
                .to_string(),
            MIGRATE
        );
        let (url, requests) = server(vec![(
            "200 OK",
            r#"{"access_token":"fixture-secret"}"#.into(),
        )]);
        assert!(exchange_code(
            &url,
            &Authorization {
                code: "code".into(),
                client_id: "client".into()
            },
            "verifier"
        )
        .is_err());
        requests.join().unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("credentials/openai.json")).unwrap(),
            legacy
        );
        // Only a fully validated new grant replaces the legacy token fields.
        let credential =
            credential_from_token(&serde_json::from_str(TOKEN).unwrap(), "client", None).unwrap();
        write_credential(&dir, &credential).unwrap();
        let stored = read_credential(&dir).unwrap();
        assert_eq!(stored.access_token, "new-secret");
        assert_eq!(stored.refresh_token, "rotated-secret");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explicit_catalog_fetch_sends_only_bearer_and_decodes_platform_data() {
        let dir = credential_dir();
        let (url, requests) = server(vec![(
            "200 OK",
            r#"{"data":[{"id":"model-b"},{"id":"model-a"}]}"#.into(),
        )]);
        assert_eq!(
            refresh_at(&dir, &format!("{url}/v1/models"), "http://127.0.0.1:1").unwrap(),
            ["model-b", "model-a"]
        );
        let requests = requests.join().unwrap();
        assert!(requests[0].starts_with("GET /v1/models HTTP/1.1"));
        assert!(requests[0].contains("Authorization: Bearer fixture-secret\r\n"));
        assert!(!requests[0]
            .to_ascii_lowercase()
            .contains("chatgpt-account-id"));
        assert_eq!(
            catalog_source(OPENAI_ORIGIN).unwrap(),
            "https://api.openai.com/v1/models"
        );
        for conflicting in [
            "https://chatgpt.com/backend-api",
            "https://proxy.invalid/v1",
            "http://127.0.0.1:1/v1",
            "https://api.openai.com/v1?other",
        ] {
            assert!(refresh(&dir, conflicting)
                .unwrap_err()
                .to_string()
                .contains("conflicting"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explicit_catalog_fetch_decodes_all_chatgpt_slugs_in_reported_order() {
        let dir = credential_dir();
        let (url, requests) = server(vec![(
            "200 OK",
            r#"{"models":[
                {"slug":"model-b","visibility":"visible","minimal_client_version":"1.0.0","display_name":"Model B"},
                {"slug":"hidden-model","visibility":"hidden","minimal_client_version":"99.0.0"},
                {"slug":"namespace/model-a","visibility":"visible","capabilities":{"tools":true}}
            ]}"#.into(),
        )]);
        assert_eq!(
            refresh_at(&dir, &format!("{url}/v1/models"), "http://127.0.0.1:1").unwrap(),
            ["model-b", "hidden-model", "namespace/model-a"]
        );
        let requests = requests.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /v1/models HTTP/1.1"));
        assert!(requests[0].contains("Authorization: Bearer fixture-secret\r\n"));
        assert!(!requests[0]
            .to_ascii_lowercase()
            .contains("chatgpt-account-id"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unauthorized_refreshes_once_rotates_and_retries_using_dynamic_client() {
        verify_inference_authentication();
        let dir = credential_dir();
        let (url, requests) = server(vec![
            ("401 Unauthorized", "private response".into()),
            ("200 OK", TOKEN.into()),
            (
                "200 OK",
                r#"{"data":[{"id":"model-after-renewal"}]}"#.into(),
            ),
        ]);
        assert_eq!(
            refresh_at(&dir, &format!("{url}/models"), &format!("{url}/token")).unwrap(),
            ["model-after-renewal"]
        );
        let requests = requests.join().unwrap();
        for field in [
            "grant_type=refresh_token",
            "client_id=issued-fixture-client",
            "refresh_token=refresh-secret",
            "resource=https%3A%2F%2Fapi.openai.com%2Fv1",
        ] {
            assert!(requests[1].contains(field));
        }
        assert!(requests[2].contains("Authorization: Bearer new-secret"));
        assert_eq!(
            read_credential(&dir).unwrap().refresh_token,
            "rotated-secret"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    fn verify_inference_authentication() {
        for expired in [false, true] {
            for final_status in ["200 OK", "401 Unauthorized"] {
                let dir = credential_dir();
                if expired {
                    let mut credential = read_credential(&dir).unwrap();
                    credential.expires_at = now() - 1;
                    write_credential(&dir, &credential).unwrap();
                }
                let mut replies = vec![];
                if !expired {
                    replies.push(("401 Unauthorized", "private upstream text".into()));
                }
                replies.extend([
                    ("200 OK", TOKEN.into()),
                    (final_status, "fixture body".into()),
                ]);
                let (url, requests) = server(replies);
                let result = fixture_inference_response(
                    &dir,
                    b"{\"store\":false}",
                    &format!("{url}/responses"),
                    &format!("{url}/token"),
                );
                if final_status == "200 OK" {
                    assert_eq!(result.unwrap().status(), 200);
                } else {
                    assert_eq!(result.err().unwrap().to_string(), EXPIRED);
                }
                let requests = requests.join().unwrap();
                assert_eq!(requests.len(), if expired { 2 } else { 3 });
                let token_index = usize::from(!expired);
                assert!(requests[token_index].starts_with("POST /token"));
                assert!(requests[token_index].contains("client_id=issued-fixture-client"));
                assert!(
                    requests[token_index].contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1")
                );
                let inference = &requests[token_index + 1];
                assert!(inference.starts_with("POST /responses"));
                assert!(inference.contains("Authorization: Bearer new-secret"));
                assert!(inference.ends_with("{\"store\":false}"));
                assert_eq!(
                    read_credential(&dir).unwrap().refresh_token,
                    "rotated-secret"
                );
                fs::remove_dir_all(dir).unwrap();
            }
        }
        for status in ["400 Bad Request", "302 Found", "500 Internal Server Error"] {
            let dir = credential_dir();
            let path = dir.join("credentials/openai.json");
            let before = fs::read(&path).unwrap();
            let (url, requests) = server(vec![
                ("401 Unauthorized", "private upstream text".into()),
                (status, "private upstream text".into()),
            ]);
            let error = fixture_inference_response(
                &dir,
                b"{}",
                &format!("{url}/responses"),
                &format!("{url}/token"),
            )
            .err()
            .unwrap()
            .to_string();
            assert!(!error.contains("private upstream text"));
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(requests.join().unwrap().len(), 2);
            fs::remove_dir_all(dir).unwrap();
        }
        for status in ["302 Found", "403 Forbidden", "500 Internal Server Error"] {
            let dir = credential_dir();
            let before = fs::read(dir.join("credentials/openai.json")).unwrap();
            let (url, requests) = server(vec![(status, "private upstream text".into())]);
            let error = fixture_inference_response(&dir, b"{}", &url, "http://127.0.0.1:1/token")
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("private upstream text"));
            assert!(error.contains(&status[..3]));
            assert_eq!(requests.join().unwrap().len(), 1);
            assert_eq!(
                fs::read(dir.join("credentials/openai.json")).unwrap(),
                before
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn expiry_refreshes_before_fetch_and_auth_failure_after_renewal_is_bounded() {
        for expired in [false, true] {
            let dir = credential_dir();
            let mut credential = read_credential(&dir).unwrap();
            if expired {
                credential.expires_at = now() - 1;
                write_credential(&dir, &credential).unwrap();
            }
            let mut replies = vec![];
            if !expired {
                replies.push(("401 Unauthorized", "private response".into()));
            }
            replies.extend([
                ("200 OK", r#"{"access_token":"new-secret","expires_in":3600,"scope":"chatgpt.tokens.use.direct"}"#.into()),
                ("401 Unauthorized", "private response".into()),
            ]);
            let (url, requests) = server(replies);
            assert_eq!(
                refresh_at(&dir, &url, &format!("{url}/token"))
                    .unwrap_err()
                    .to_string(),
                EXPIRED
            );
            let requests = requests.join().unwrap();
            assert_eq!(requests.len(), if expired { 2 } else { 3 });
            if expired {
                assert!(requests[0].starts_with("POST /token"));
            }
            assert_eq!(
                read_credential(&dir).unwrap().refresh_token,
                "refresh-secret"
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn catalog_errors_do_not_echo_secrets_or_replace_last_good_and_curated_models() {
        let dir = credential_dir();
        fs::create_dir_all(dir.join("catalogs")).unwrap();
        fs::write(dir.join("catalogs/openai.json"), "[\"cached\"]\n").unwrap();
        fs::write(dir.join("models.json"), format!(r#"{{"providers":{{"openai":{{"baseUrl":"{OPENAI_ORIGIN}","api":"openai-completions","models":[{{"id":"curated"}}]}}}}}}"#)).unwrap();
        for (status, body, expected) in [
            ("403 Forbidden", "fixture-secret", "permission denied"),
            ("302 Found", "fixture-secret", "redirects are not followed"),
            ("500 Internal Server Error", "fixture-secret", "HTTP 500"),
            (
                "200 OK",
                r#"{"data":[{"id":"bad\nfixture-secret"}]}"#,
                "invalid OpenAI model id",
            ),
            (
                "200 OK",
                r#"{"data":[{"id":true}],"fixture-secret":1}"#,
                "invalid OpenAI model catalog",
            ),
            (
                "200 OK",
                r#"{"models":{"slug":"fixture-secret"}}"#,
                "invalid OpenAI model catalog",
            ),
            (
                "200 OK",
                r#"{"models":[{"slug":true}],"fixture-secret":1}"#,
                "invalid OpenAI model catalog",
            ),
            (
                "200 OK",
                r#"{"models":[{"id":"fixture-secret"}]}"#,
                "invalid OpenAI model catalog",
            ),
            (
                "200 OK",
                r#"{"models":[{"slug":"bad\nfixture-secret"}]}"#,
                "invalid OpenAI model id",
            ),
            (
                "200 OK",
                r#"{"models":[{"slug":""}],"fixture-secret":1}"#,
                "invalid OpenAI model id",
            ),
        ] {
            let (url, requests) = server(vec![(status, body.into())]);
            let error = refresh_at(&dir, &url, "http://127.0.0.1:1")
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("fixture-secret"));
            requests.join().unwrap();
            assert_eq!(
                fs::read(dir.join("catalogs/openai.json")).unwrap(),
                b"[\"cached\"]\n"
            );
            assert_eq!(
                load_registry(&dir).unwrap().providers[0].models[0].id,
                "curated"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn token_errors_caps_and_invalid_grants_preserve_credential() {
        let dir = credential_dir();
        let original = fs::read(dir.join("credentials/openai.json")).unwrap();
        for (status, body, expected) in [
            (
                "302 Found",
                "fixture-secret".into(),
                "redirects are not followed",
            ),
            ("400 Bad Request", "fixture-secret".into(), "HTTP 400"),
            (
                "200 OK",
                "{fixture-secret".into(),
                "invalid OpenAI token response",
            ),
            (
                "200 OK",
                "x".repeat(BODY_CAP as usize + 1),
                "larger than 1 MiB",
            ),
            (
                "200 OK",
                r#"{"access_token":"fixture-secret","expires_in":3600,"scope":"openid"}"#.into(),
                "invalid OpenAI token response",
            ),
        ] {
            let (url, requests) = server(vec![(status, body)]);
            let error = renew_token(&url, &read_credential(&dir).unwrap())
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("fixture-secret"));
            requests.join().unwrap();
            assert_eq!(
                fs::read(dir.join("credentials/openai.json")).unwrap(),
                original
            );
        }
        let (url, requests) = server(vec![("200 OK", "x".repeat(BODY_CAP as usize + 1))]);
        assert!(refresh_at(&dir, &url, "http://127.0.0.1:1")
            .unwrap_err()
            .to_string()
            .contains("larger than 1 MiB"));
        requests.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn token_contract_rejects_invalid_expiry_scopes_and_control_characters() {
        for (field, value) in [
            ("expires_in", serde_json::json!(0)),
            ("expires_in", serde_json::json!(-1)),
            ("expires_in", serde_json::json!(1.5)),
            ("expires_in", serde_json::json!(u64::MAX)),
            ("scope", serde_json::json!("openid")),
            ("access_token", serde_json::json!("secret\nheader")),
            ("refresh_token", serde_json::json!("")),
        ] {
            let mut token: Value = serde_json::from_str(TOKEN).unwrap();
            token[field] = value;
            assert!(credential_from_token(&token, "client", None).is_err());
        }
    }

    #[test]
    fn credential_write_failure_keeps_old_file_and_foreign_temporary_file() {
        let dir = credential_dir();
        let original = fs::read(dir.join("credentials/openai.json")).unwrap();
        fs::write(dir.join("credentials/openai.json.tmp"), "other writer").unwrap();
        let credential =
            credential_from_token(&serde_json::from_str(TOKEN).unwrap(), "client", None).unwrap();
        assert!(write_credential(&dir, &credential).is_err());
        assert_eq!(
            fs::read(dir.join("credentials/openai.json")).unwrap(),
            original
        );
        assert_eq!(
            fs::read_to_string(dir.join("credentials/openai.json.tmp")).unwrap(),
            "other writer"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
