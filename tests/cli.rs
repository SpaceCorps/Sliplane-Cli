//! Drives the built binary against an in-process mock of the Sliplane API. Every test gets its
//! own config directory and the plaintext store, so nothing touches a real keystore or account.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

type Route = (&'static str, &'static str, u16, Value);

struct Mock {
    url: String,
    log: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    /// Routes are (method, path-with-query, status, body). Unmatched requests get a 404.
    fn start(routes: Vec<Route>) -> Mock {
        Mock::start_on("127.0.0.1:0", routes)
    }

    /// `start`, on an address of your choosing (`[::1]:0` for an IPv6-only mock).
    fn start_on(bind: &str, routes: Vec<Route>) -> Mock {
        let listener = TcpListener::bind(bind).unwrap();
        let url = format!("http://{}/v0/", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let routes = routes.clone();
                let log = log2.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").trim_start_matches("/v0/").to_string();
                    let mut headers = Vec::new();
                    let mut len = 0usize;
                    loop {
                        let mut h = String::new();
                        reader.read_line(&mut h).unwrap();
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            let (k, v) = (k.trim().to_lowercase(), v.trim().to_string());
                            if k == "content-length" {
                                len = v.parse().unwrap_or(0);
                            }
                            headers.push((k, v));
                        }
                    }
                    let mut buf = vec![0; len];
                    reader.read_exact(&mut buf).unwrap();
                    let body = (len > 0).then(|| serde_json::from_slice(&buf).unwrap());
                    log.lock().unwrap().push(Recorded { method: method.clone(), path: path.clone(), headers, body });

                    // How many times this route has been asked, this request included.
                    let nth = log.lock().unwrap().iter().filter(|r| r.method == method && r.path == path).count();
                    let (status, resp) = routes
                        .iter()
                        // A route ending in `*` matches any path that starts with what is before it.
                        .find(|(m, p, _, _)| {
                            *m == method && (*p == path || p.strip_suffix('*').is_some_and(|pre| path.starts_with(pre)))
                        })
                        .map(|(_, _, s, b)| (*s, b.clone()))
                        .unwrap_or((404, json!({"code": "not_found", "message": "no route"})));
                    // `{"__seq": [a, b, c]}` answers a, then b, then c for every call after.
                    let resp = match resp.get("__seq").and_then(Value::as_array) {
                        Some(seq) => seq[(nth - 1).min(seq.len() - 1)].clone(),
                        None => resp,
                    };
                    // `{"__headers": {"Name": "value"}, "__body": ...}` sends those response headers
                    // along with the body (an object with no `__body` has an empty one).
                    let (resp, extra_headers) = match resp.get("__headers").and_then(Value::as_object) {
                        Some(h) => (
                            resp.get("__body").cloned().unwrap_or_else(|| json!({})),
                            h.iter().map(|(k, v)| format!("{k}: {}\r\n", v.as_str().unwrap_or(""))).collect::<String>(),
                        ),
                        None => (resp, String::new()),
                    };
                    let text = if status == 204 { String::new() } else { resp.to_string() };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    );
                });
            }
        });
        Mock { url, log }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    fn last(&self, method: &str) -> Recorded {
        self.requests().into_iter().rev().find(|r| r.method == method).expect("no such request")
    }
}

struct Env {
    dir: PathBuf,
    api: String,
}

impl Env {
    fn new(mock: &Mock) -> Env {
        // A counter, not the clock: macOS time has microsecond resolution, and two parallel tests
        // sharing a directory delete each other's config on drop.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sliplane-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Env { dir, api: mock.url.clone() }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sliplane"))
            .args(args)
            .env("SLIPLANE_CONFIG_DIR", &self.dir)
            .env("SLIPLANE_SECRET_STORE", "plaintext")
            .env("SLIPLANE_API_URL", &self.api)
            .env("SLIPLANE_PRESETS_URL", format!("{}presets", self.api))
            .output()
            .unwrap()
    }

    /// `run`, with more environment variables, and none of the ambient `NO_PROXY` ones.
    fn run_with(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sliplane"))
            .args(args)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env("SLIPLANE_CONFIG_DIR", &self.dir)
            .env("SLIPLANE_SECRET_STORE", "plaintext")
            .env("SLIPLANE_API_URL", &self.api)
            .envs(envs.iter().copied())
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> (i32, Value, Value) {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.run(&all);
        let parse = |b: &[u8]| {
            let s = String::from_utf8_lossy(b);
            // The plaintext store warns, and name lookups note what they chose, on stderr before any envelope.
            let s = s
                .lines()
                .filter(|l| !l.starts_with("warning:") && !l.starts_with("note:"))
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::from_str(&s).unwrap_or(Value::Null)
        };
        (out.status.code().unwrap(), parse(&out.stdout), parse(&out.stderr))
    }

    /// Adds account `work` with key `sl_test`, verified against the mock's `me`.
    fn with_account(self) -> Env {
        let (code, out, err) = self.json(&["accounts", "add", "work", "--api-key", "sl_test"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out["status"], "added");
        self
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn me() -> Route {
    (
        "GET",
        "me",
        200,
        json!({"authType": "apikey", "tokenType": "rw", "organizationId": "org_1", "user": {"email": "dev@example.com"}}),
    )
}

const SVC: &str = "projects/p1/services/s1";

#[test]
fn accounts_lifecycle() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();

    assert_eq!(mock.last("GET").headers.iter().find(|(k, _)| k == "authorization").unwrap().1, "Bearer sl_test");

    let (code, out, _) = env.json(&["accounts", "list"]);
    assert_eq!(code, 0);
    assert_eq!(out["count"], 1);
    assert_eq!(out["accounts"][0]["identity"], "dev@example.com");
    assert_eq!(out["accounts"][0]["organization"], "org_1");
    assert_eq!(out["accounts"][0]["keyStatus"], "stored");
    assert_eq!(out["secretStore"], "plaintext");

    let (_, out, _) = env.json(&["accounts", "list", "--check"]);
    assert_eq!(out["accounts"][0]["keyStatus"], "valid");

    // Lookup is case-insensitive, and a duplicate needs --force.
    let (code, _, err) = env.json(&["accounts", "add", "WORK", "--api-key", "x"]);
    assert_eq!(code, 6);
    assert!(err["remediation"].as_str().unwrap().contains("--force"));

    let (code, out, _) = env.json(&["accounts", "test", "Work"]);
    assert_eq!(code, 0);
    assert_eq!(out["keyStatus"], "valid");

    // No terminal and no --yes: refuse rather than hang.
    let (code, _, err) = env.json(&["accounts", "remove", "work"]);
    assert_eq!(code, 6);
    assert_eq!(err["remediation"], "sliplane accounts remove work --yes");

    let (code, out, _) = env.json(&["accounts", "remove", "work", "--yes"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "removed");
    let (_, out, _) = env.json(&["accounts", "list"]);
    assert_eq!(out["count"], 0);
}

#[test]
fn api_key_from_stdin() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sliplane"))
        .args(["accounts", "add", "piped", "--api-key-stdin", "--json"])
        .env("SLIPLANE_CONFIG_DIR", &env.dir)
        .env("SLIPLANE_SECRET_STORE", "plaintext")
        .env("SLIPLANE_API_URL", &env.api)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"sl_piped\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let auth = mock.last("GET").headers.into_iter().find(|(k, _)| k == "authorization").unwrap().1;
    assert_eq!(auth, "Bearer sl_piped");
}

#[test]
fn config_is_readable_yaml_without_secrets() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let yaml = std::fs::read_to_string(env.dir.join("config.yaml")).unwrap();
    assert!(yaml.contains("work:"), "{yaml}");
    assert!(yaml.contains("identity: dev@example.com"), "{yaml}");
    assert!(!yaml.contains("sl_test"), "the key leaked into config.yaml");
}

#[test]
fn account_is_required() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&["projects", "list"]);
    assert_eq!(code, 7);
    assert_eq!(err["code"], "no_account");
    assert!(err["detail"].as_str().unwrap().contains("work (dev@example.com)"));

    let (code, _, err) = env.json(&["projects", "list", "-a", "nope"]);
    assert_eq!(code, 7);
    assert_eq!(err["remediation"], "sliplane accounts list");
}

#[test]
fn http_errors_map_to_exit_codes() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects", 403, json!({"code": "read_only_token", "message": "Read-only"})),
        ("GET", "servers", 429, json!({"code": "rate_limit_exceeded", "message": "slow down"})),
        ("GET", "postgres", 500, json!({"code": "internal_server_error", "message": "boom"})),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&["projects", "create", "--name", "x", "-a", "work"]);
    assert_eq!(code, 3);
    assert!(err["error"].as_str().unwrap().contains("read-only"));
    assert!(err["detail"].as_str().unwrap().contains("HTTP 403"));

    assert_eq!(env.json(&["servers", "list", "-a", "work"]).0, 5);
    assert_eq!(env.json(&["postgres", "list", "-a", "work"]).0, 2);
    assert_eq!(env.json(&["servers", "get", "--server-id", "nope", "-a", "work"]).0, 4);
}

#[test]
fn parse_errors_are_envelopes() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let (code, _, err) = env.json(&["servers", "get", "-a", "work"]);
    assert_eq!(code, 6);
    assert_eq!(err["code"], "invalid_input");
    assert!(err["error"].as_str().unwrap().contains("--server-id"), "{err}");

    let out = env.run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn yaml_is_the_default() {
    let mock = Mock::start(vec![me(), ("GET", "projects", 200, json!([{"id": "p1", "name": "Main"}]))]);
    let env = Env::new(&mock).with_account();
    let out = env.run(&["projects", "list", "-a", "work"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("- id: p1"), "{stdout}");
    assert!(stdout.contains("name: Main"), "{stdout}");
}

#[test]
fn deletes_print_a_parseable_status() {
    let mock = Mock::start(vec![me(), ("DELETE", SVC, 204, json!(null))]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "delete", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, json!({"status": "deleted", "projectId": "p1", "serviceId": "s1"}));
}

#[test]
fn deploy_sends_an_empty_object() {
    let mock = Mock::start(vec![me(), ("POST", "projects/p1/services/s1/deploy", 202, json!(null))]);
    let env = Env::new(&mock).with_account();
    env.json(&["services", "deploy", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    let req = mock.last("POST");
    assert_eq!(req.body, Some(json!({})));
    assert!(req.headers.iter().any(|(k, v)| k == "content-type" && v == "application/json"));
}

#[test]
fn ids_are_escaped_in_paths() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    env.json(&["services", "delete-env", "--project-id", "p1", "--service-id", "s1", "--key", "A/B C", "-a", "work"]);
    assert_eq!(mock.last("DELETE").path, "projects/p1/services/s1/env/A%2FB%20C");
}

fn repo_service() -> Value {
    json!({
        "id": "s1",
        "deployment": {"url": "https://github.com/o/r", "branch": "develop", "dockerfilePath": "docker/Dockerfile",
                       "dockerContext": "app", "autoDeploy": false, "includePaths": ["src/**"]},
        "env": [{"key": "A", "value": "1", "secret": false},
                {"key": "B", "value": "2", "secret": false},
                {"key": "S", "value": "", "secret": true}]
    })
}

#[test]
fn update_carries_the_deployment_over() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--healthcheck",
        "/health",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("PATCH").body.unwrap();
    assert_eq!(body["healthcheck"], "/health");
    assert_eq!(
        body["deployment"],
        json!({"url": "https://github.com/o/r", "branch": "develop", "dockerfilePath": "docker/Dockerfile",
               "dockerContext": "app", "autoDeploy": false, "includePaths": ["src/**"]})
    );
    assert!(body.get("env").is_none());
}

#[test]
fn update_fails_when_a_branch_does_not_take() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["services", "update", "--project-id", "p1", "--service-id", "s1", "--branch", "main", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("branch 'develop' instead of 'main'"), "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap()["deployment"]["branch"], "main");
}

#[test]
fn update_refuses_to_drop_env() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["services", "update", "--project-id", "p1", "--service-id", "s1", "--env", "A=9", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("2 environment variable(s) not listed: B, S"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "PATCH"));
}

#[test]
fn update_merges_env_and_keeps_secrets() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--env",
        "A=9",
        "--merge-env",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("PATCH").body.unwrap();
    assert_eq!(
        body["env"],
        json!([{"key": "B", "value": "2", "secret": false},
               {"key": "S", "value": "", "secret": true},
               {"key": "A", "value": "9", "secret": false}])
    );
    // The service is fetched once, however many things need it.
    assert_eq!(mock.requests().iter().filter(|r| r.method == "GET" && r.path == SVC).count(), 1);
}

#[test]
fn update_replace_env_sends_exactly_what_was_given() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, _) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--env",
        "A=9",
        "--replace-env",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0);
    assert_eq!(mock.last("PATCH").body.unwrap()["env"], json!([{"key": "A", "value": "9", "secret": false}]));
}

#[test]
fn update_refuses_repo_flags_on_an_image_service() {
    let image = json!({"id": "s1", "deployment": {"url": "ghcr.io/o/app:1", "registryAuthenticationId": "cred_1"}});
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, image.clone()), ("PATCH", SVC, 200, image)]);
    let env = Env::new(&mock).with_account();
    let (code, _, _) =
        env.json(&["services", "update", "--project-id", "p1", "--service-id", "s1", "--branch", "x", "-a", "work"]);
    assert_eq!(code, 6);

    // A new tag in the same registry keeps the credentials.
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--image",
        "ghcr.io/o/app:2",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PATCH").body.unwrap()["deployment"],
        json!({"url": "ghcr.io/o/app:2", "registryAuthenticationId": "cred_1"})
    );
}

#[test]
fn update_refuses_windows_paths() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--healthcheck",
        "C:/Program Files/Git/",
        "-a",
        "work",
    ]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("Windows path"));
}

#[test]
fn create_requires_a_deployment_and_resolves_volume_names() {
    let mock = Mock::start(vec![
        me(),
        (
            "GET",
            "servers/srv1/volumes",
            200,
            json!([{"id": "volume_a", "name": "data"}, {"id": "volume_b", "name": "data"}]),
        ),
        ("POST", "projects/p1/services", 201, json!({"id": "s9"})),
    ]);
    let env = Env::new(&mock).with_account();
    let base = ["services", "create", "--project-id", "p1", "--name", "web", "--server-id", "srv1", "-a", "work"];

    let (code, _, err) = env.json(&base);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("--image"));

    let mut args = base.to_vec();
    args.extend([
        "--image",
        "nginx:latest",
        "--public",
        "--protocol",
        "http",
        "--env",
        "A=1",
        "--secret-env",
        "S=2",
        "--volume",
        "data:/data",
        "--volume",
        "fresh:/cache",
        "--volume",
        "volume_z:/z",
    ]);
    let (code, out, err) = env.json(&args);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s9");
    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["deployment"], json!({"url": "nginx:latest"}));
    assert_eq!(body["network"], json!({"public": true, "protocol": "http"}));
    assert_eq!(
        body["volumes"],
        json!([{"id": "volume_a", "mountPath": "/data"}, {"name": "fresh", "mountPath": "/cache"},
               {"id": "volume_z", "mountPath": "/z"}])
    );
    assert_eq!(body["env"][1], json!({"key": "S", "value": "2", "secret": true}));

    let mut args = base.to_vec();
    args.extend(["--image", "nginx", "--protocol", "tcp"]);
    assert_eq!(env.json(&args).0, 6, "--protocol without --public");
}

#[test]
fn services_list_walks_every_project_in_order() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "projects", 200, json!([{"id": "p1"}, {"id": "p2"}, {"id": "p3"}])),
        ("GET", "projects/p1/services", 200, json!([{"id": "a"}])),
        ("GET", "projects/p2/services", 200, json!([])),
        ("GET", "projects/p3/services", 200, json!([{"id": "b"}, {"id": "c"}])),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, _) = env.json(&["services", "list", "-a", "work"]);
    assert_eq!(code, 0);
    assert_eq!(out, json!([{"id": "a"}, {"id": "b"}, {"id": "c"}]));
}

#[test]
fn postgres_and_bucket_bodies() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "postgres", 201, json!({"id": "pg1"})),
        ("PUT", "buckets/b1/cors", 200, json!({"rules": []})),
        ("GET", "postgres/pg1/logs?since=2026-09-01T00%3A00%3A00Z&limit=5", 200, json!([])),
        ("GET", "servers/srv1/metrics?range=1h", 200, json!([])),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&[
        "postgres",
        "create",
        "--name",
        "db",
        "--instance-type",
        "base",
        "--region",
        "ger",
        "--pg-version",
        "18",
        "--ip-allow",
        "203.0.113.0/24=office",
        "--ip-allow",
        "198.51.100.1/32",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("POST").body.unwrap(),
        json!({"name": "db", "instanceType": "base", "region": "ger", "version": 18,
               "ipAllowList": [{"source": "203.0.113.0/24", "description": "office"}, {"source": "198.51.100.1/32"}]})
    );

    let (code, _, err) = env.json(&[
        "buckets",
        "set-cors",
        "--bucket-id",
        "b1",
        "--allowed-origin",
        "*",
        "--allowed-method",
        "get",
        "--max-age",
        "60",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PUT").body.unwrap(),
        json!({"rules": [{"allowedOrigins": ["*"], "allowedMethods": ["GET"], "maxAgeSeconds": 60}]})
    );

    let (code, _, err) = env.json(&[
        "postgres",
        "logs",
        "--postgres-id",
        "pg1",
        "--since",
        "2026-09-01T00:00:00Z",
        "--limit",
        "5",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(env.json(&["servers", "metrics", "--server-id", "srv1", "-a", "work"]).0, 0);
    assert_eq!(env.json(&["postgres", "logs", "--postgres-id", "pg1", "--limit", "5000", "-a", "work"]).0, 6);
}

#[test]
fn agent_readme_as_data() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let (code, out, _) = env.json(&["agent-readme"]);
    assert_eq!(code, 0);
    assert_eq!(out["exitCodes"]["7"], "no_account - run sliplane accounts list");
    let md = String::from_utf8(env.run(&["agent-readme"]).stdout).unwrap();
    assert!(md.starts_with("# sliplane - agent operating manual"));
}

#[test]
fn login_command_lifecycle() {
    let mock = Mock::start(vec![me(), me(), me()]);
    let env = Env::new(&mock);

    // Login with default name "default"
    let (code, out, err) = env.json(&["login", "--api-key", "sl_login_test"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "logged_in");
    assert_eq!(out["name"], "default");
    assert_eq!(out["identity"], "dev@example.com");
    assert_eq!(out["organization"], "org_1");
    assert_eq!(out["secretStore"], "plaintext");

    // Re-login without --force fails
    let (code, _, err) = env.json(&["login", "--api-key", "sl_new_key"]);
    assert_eq!(code, 6);
    assert!(err["remediation"].as_str().unwrap().contains("--force"));

    // Re-login with --force succeeds
    let (code, out, _) = env.json(&["login", "--api-key", "sl_new_key", "--force"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "logged_in");

    // Login with named account
    let (code, out, _) = env.json(&["login", "staging", "--api-key", "sl_staging"]);
    assert_eq!(code, 0);
    assert_eq!(out["name"], "staging");
}

// ---------------------------------------------------------------------------------------------
// API 0.10.0: scoped keys, SSH keys, service volumes, Postgres metrics, pricing

/// `me` as a key with explicit scopes answers it - what `GET /me` returns since API 0.10.0.
fn me_with_scopes(scopes: Value) -> Route {
    (
        "GET",
        "me",
        200,
        json!({"authType": "apikey", "tokenType": "rw", "organizationId": "org_1", "scopes": scopes,
               "user": {"email": "dev@example.com"}}),
    )
}

fn merge_env_args() -> Vec<&'static str> {
    vec!["services", "update", "--project-id", "p1", "--service-id", "s1", "--env", "A=9", "--merge-env", "-a", "work"]
}

#[test]
fn merge_env_refuses_when_the_key_cannot_read_secrets() {
    // Without secrets:read every value reads back empty, and a plain variable sent back empty is
    // overwritten with nothing. The secret S would have been safe; B would not.
    let blind = json!({"id": "s1", "deployment": repo_service()["deployment"].clone(),
        "env": [{"key": "A", "value": "", "secret": false}, {"key": "B", "value": "", "secret": false},
                {"key": "S", "value": "", "secret": true}]});
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:read", "services:write"])),
        ("GET", SVC, 200, blind),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&merge_env_args());
    assert_eq!(code, 3, "{err}");
    assert_eq!(err["code"], "auth_required");
    let message = err["error"].as_str().unwrap();
    assert!(message.contains("secrets:read") && message.contains("1 variable(s)") && message.contains(": B."), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("set-env"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "PATCH"), "nothing may be sent");
}

#[test]
fn merge_env_without_the_secrets_scope_is_fine_when_only_secrets_are_carried() {
    // Secrets go back empty to mean "keep the stored value", which needs no read access.
    let only_secret = json!({"id": "s1", "deployment": repo_service()["deployment"].clone(),
        "env": [{"key": "A", "value": "", "secret": false}, {"key": "S", "value": "", "secret": true}]});
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:read", "services:write"])),
        ("GET", SVC, 200, only_secret),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&merge_env_args());
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PATCH").body.unwrap()["env"],
        json!([{"key": "S", "value": "", "secret": true}, {"key": "A", "value": "9", "secret": false}])
    );
}

#[test]
fn merge_env_proceeds_with_the_secrets_scope() {
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:read", "services:write", "secrets:read"])),
        ("GET", SVC, 200, repo_service()),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&merge_env_args());
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap()["env"][0], json!({"key": "B", "value": "2", "secret": false}));
}

#[test]
fn accounts_warn_when_the_key_has_no_secrets_scope() {
    let mock = Mock::start(vec![me_with_scopes(json!(["services:read"]))]);
    let env = Env::new(&mock);
    let (code, out, err) = env.json(&["accounts", "add", "work", "--api-key", "sl_test"]);
    assert_eq!(code, 0, "{err}");
    assert!(out["warning"].as_str().unwrap().contains("secrets:read"), "{out}");
    let (_, out, _) = env.json(&["accounts", "test", "work"]);
    assert!(out["warning"].as_str().unwrap().contains("secrets:read"), "{out}");
    assert_eq!(out["me"]["scopes"], json!(["services:read"]));

    // A key that reports no scopes at all (legacy) and one that has the scope: no warning.
    let mock = Mock::start(vec![me()]);
    let (_, out, _) = Env::new(&mock).json(&["accounts", "add", "work", "--api-key", "sl_test"]);
    assert!(out.get("warning").is_none(), "{out}");
    let mock = Mock::start(vec![me_with_scopes(json!(["secrets:read"]))]);
    let (_, out, _) = Env::new(&mock).json(&["accounts", "add", "work", "--api-key", "sl_test"]);
    assert!(out.get("warning").is_none(), "{out}");
}

#[test]
fn ssh_key_commands() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "ssh-keys", 200, json!([{"id": "sshkey_1"}])),
        ("POST", "ssh-keys", 201, json!({"id": "sshkey_1", "fingerprint": "SHA256:x"})),
        ("DELETE", "ssh-keys/sshkey_1", 204, json!(null)),
        ("GET", "projects/p1/services/s1/ssh-keys", 200, json!([{"keyId": "sshkey_1"}])),
        ("PUT", "projects/p1/services/s1/ssh-keys/sshkey_1", 200, json!({"host": "h", "port": 2222, "username": "u"})),
        ("DELETE", "projects/p1/services/s1/ssh-keys/sshkey_1", 204, json!(null)),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, out, _) = env.json(&["ssh-keys", "list", "-a", "work"]);
    assert_eq!((code, out), (0, json!([{"id": "sshkey_1"}])));

    let (code, _, err) = env.json(&[
        "ssh-keys",
        "create",
        "--name",
        "laptop",
        "--public-key",
        "ssh-ed25519 AAAAC3 me@laptop\n",
        "--expires-at",
        "2026-12-01T00:00:00Z",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("POST").body.unwrap(),
        json!({"name": "laptop", "publicKey": "ssh-ed25519 AAAAC3 me@laptop", "expiresAt": "2026-12-01T00:00:00Z"})
    );

    // From a file, trimmed; no expiry key unless asked for.
    let file = env.dir.join("id.pub");
    std::fs::write(&file, "ssh-ed25519 BBBB me@desk\r\n").unwrap();
    let (code, _, err) =
        env.json(&["ssh-keys", "create", "--name", "desk", "--public-key-file", file.to_str().unwrap(), "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("POST").body.unwrap(), json!({"name": "desk", "publicKey": "ssh-ed25519 BBBB me@desk"}));

    let (code, out, _) = env.json(&["ssh-keys", "delete", "--key-id", "sshkey_1", "-a", "work"]);
    assert_eq!((code, out), (0, json!({"status": "deleted", "keyId": "sshkey_1"})));

    let svc = ["--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let (code, out, _) = env.json(&[&["services", "ssh-keys"][..], &svc].concat());
    assert_eq!((code, out), (0, json!([{"keyId": "sshkey_1"}])));

    let (code, out, err) = env.json(&[&["services", "attach-ssh-key", "--key-id", "sshkey_1"][..], &svc].concat());
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, json!({"host": "h", "port": 2222, "username": "u"}));
    // Attaching sends no body at all, as the spec declares none.
    assert_eq!(mock.last("PUT").body, None);

    let (code, out, _) = env.json(&[&["services", "detach-ssh-key", "--key-id", "sshkey_1"][..], &svc].concat());
    assert_eq!(
        (code, out),
        (0, json!({"status": "detached", "projectId": "p1", "serviceId": "s1", "keyId": "sshkey_1"}))
    );
}

#[test]
fn ssh_key_create_refuses_a_private_key_and_a_missing_key() {
    let mock = Mock::start(vec![me(), ("POST", "ssh-keys", 201, json!({}))]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&[
        "ssh-keys",
        "create",
        "--name",
        "oops",
        "--public-key",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nabc",
        "-a",
        "work",
    ]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("private key"), "{err}");

    let (code, _, err) = env.json(&["ssh-keys", "create", "--name", "none", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("--public-key"), "{err}");

    let args = ["ssh-keys", "create", "--name", "x", "--public-key-file", "/no/such/file", "-a", "work"];
    assert_eq!(env.json(&args).0, 6);
    assert!(mock.requests().iter().all(|r| r.method != "POST"), "nothing may be sent");
}

#[test]
fn ssh_key_in_use_says_how_to_detach() {
    let mock = Mock::start(vec![
        me(),
        ("DELETE", "ssh-keys/sshkey_1", 409, json!({"code": "ssh_key_in_use", "message": "attached"})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["ssh-keys", "delete", "--key-id", "sshkey_1", "-a", "work"]);
    assert_eq!(code, 1);
    assert!(err["detail"].as_str().unwrap().contains("detach-ssh-key"), "{err}");
}

#[test]
fn service_volume_commands() {
    let volume = json!({"id": "volume_data", "name": "data", "mountPath": "/data"});
    let mock = Mock::start(vec![
        me(),
        ("GET", SVC, 200, json!({"id": "s1", "serverId": "srv1"})),
        (
            "GET",
            "servers/srv1/volumes",
            200,
            json!([{"id": "volume_data", "name": "data"}, {"id": "volume_other", "name": "other"}]),
        ),
        ("POST", "projects/p1/services/s1/volumes", 201, volume),
        (
            "PATCH",
            "projects/p1/services/s1/volumes/volume_data",
            200,
            json!({"id": "volume_data", "mountPath": "/mnt"}),
        ),
        ("DELETE", "projects/p1/services/s1/volumes/volume_data", 204, json!(null)),
        ("PATCH", "servers/srv1/volumes/volume_data", 200, json!({"id": "volume_data", "name": "renamed"})),
    ]);
    let env = Env::new(&mock).with_account();
    let svc = ["--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let run = |head: &[&str]| env.json(&[head, &svc].concat());

    // A name that matches an existing volume on the service's server mounts that one...
    let (code, _, err) = run(&["services", "attach-volume", "--volume", "data:/data"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("POST").body.unwrap(), json!({"id": "volume_data", "mountPath": "/data"}));

    // ...an unknown name creates one, and an id is used as given.
    run(&["services", "attach-volume", "--volume", "fresh:/fresh"]);
    assert_eq!(mock.last("POST").body.unwrap(), json!({"name": "fresh", "mountPath": "/fresh"}));
    run(&["services", "attach-volume", "--volume", "volume_other:/other"]);
    assert_eq!(mock.last("POST").body.unwrap(), json!({"id": "volume_other", "mountPath": "/other"}));

    let (code, _, err) = run(&["services", "attach-volume", "--volume", "data"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("id:/path or name:/path"), "{err}");

    let (code, _, err) = run(&["services", "update-volume", "--volume-id", "volume_data", "--mount-path", "/mnt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap(), json!({"mountPath": "/mnt"}));

    // Git Bash on Windows turns "/mnt" into a drive path before the CLI sees it.
    let rewritten =
        ["services", "update-volume", "--volume-id", "volume_data", "--mount-path", "C:/Program Files/Git/mnt"];
    let (code, _, err) = run(&rewritten);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("Windows path"), "{err}");
    let relative = ["services", "update-volume", "--volume-id", "volume_data", "--mount-path", "mnt"];
    assert_eq!(run(&relative).0, 6);

    let (code, out, _) = run(&["services", "detach-volume", "--volume-id", "volume_data"]);
    assert_eq!(
        (code, out),
        (0, json!({"status": "detached", "projectId": "p1", "serviceId": "s1", "volumeId": "volume_data"}))
    );

    let (code, _, err) = env.json(&[
        "servers",
        "rename-volume",
        "--server-id",
        "srv1",
        "--volume-id",
        "volume_data",
        "--name",
        "renamed",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap(), json!({"name": "renamed"}));
}

#[test]
fn metrics_take_a_range_or_a_window_never_both() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "servers/srv1/metrics?from=100&to=200", 200, json!([{"cpuUsage": 1}])),
        ("GET", "projects/p1/services/s1/metrics?range=7d", 200, json!([])),
        ("GET", "projects/p1/services/s1/metrics?range=1h", 200, json!([])),
        ("GET", "postgres/pg1/metrics?range=24h", 200, json!([{"activeConnections": 3}])),
        ("GET", "postgres/pg1/metrics?from=100&to=200", 200, json!([])),
    ]);
    let env = Env::new(&mock).with_account();
    let ok = |args: &[&str]| {
        let (code, out, err) = env.json(args);
        assert_eq!(code, 0, "{err}");
        out
    };

    // The API answers 400 to `range` together with from/to, so the 1h default must not ride along.
    ok(&["servers", "metrics", "--server-id", "srv1", "--from", "100", "--to", "200", "-a", "work"]);
    ok(&["services", "metrics", "--project-id", "p1", "--service-id", "s1", "--range", "7d", "-a", "work"]);
    ok(&["services", "metrics", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(
        ok(&["postgres", "metrics", "--postgres-id", "pg1", "--range", "24h", "-a", "work"]),
        json!([{"activeConnections": 3}])
    );
    ok(&["postgres", "metrics", "--postgres-id", "pg1", "--from", "100", "--to", "200", "-a", "work"]);

    // Contradictory or half-given windows are refused before any request.
    let before = mock.requests().len();
    let both =
        ["postgres", "metrics", "--postgres-id", "pg1", "--range", "1h", "--from", "1", "--to", "2", "-a", "work"];
    let from_only = ["postgres", "metrics", "--postgres-id", "pg1", "--from", "1", "-a", "work"];
    let to_only = ["servers", "metrics", "--server-id", "srv1", "--to", "2", "-a", "work"];
    for args in [both.as_slice(), from_only.as_slice(), to_only.as_slice()] {
        assert_eq!(env.json(args).0, 6, "{args:?}");
    }
    assert_eq!(mock.requests().len(), before);
}

#[test]
fn pricing_is_a_plain_get() {
    let mock = Mock::start(vec![me(), ("GET", "pricing", 200, json!({"currency": "EUR"}))]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["pricing", "-a", "work"]);
    assert_eq!((code, out), (0, json!({"currency": "EUR"})), "{err}");
}

#[test]
fn maintenance_mode_has_its_own_message() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects", 423, json!({"code": "organization_maintenance", "message": "locked"})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["projects", "create", "--name", "x", "-a", "work"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("maintenance"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("maintenance"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// API 0.11.0: deploy hook rotation

const ROTATE: &str = "projects/p1/services/s1/deploy-hook/rotate";

#[test]
fn rotate_deploy_hook_sends_no_body_and_warns_that_the_old_url_is_dead() {
    let mock = Mock::start(vec![
        me(),
        ("POST", ROTATE, 200, json!({"deployHookUrl": "https://api.sliplane.io/deploy/s1/dhs_new"})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) =
        env.json(&["services", "rotate-deploy-hook", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["deployHookUrl"], "https://api.sliplane.io/deploy/s1/dhs_new");
    assert!(out["note"].as_str().unwrap().contains("stopped working immediately"), "{out}");
    assert_eq!(mock.last("POST").body, None, "the spec declares no request body");
}

#[test]
fn a_missing_scope_is_not_reported_as_a_generic_403() {
    let mock = Mock::start(vec![
        me(),
        (
            "POST",
            ROTATE,
            403,
            json!({"code": "insufficient_scope", "message": "This token is missing a required scope"}),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["services", "rotate-deploy-hook", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 3);
    assert_eq!(err["code"], "auth_required");
    assert!(err["error"].as_str().unwrap().contains("missing a permission"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("sliplane me"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("insufficient_scope"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// presets

fn catalog() -> Route {
    (
        "GET",
        "presets",
        200,
        json!({
            "generatedValues": [
                {"placeholder": "__SLIPLANE_GENERATED_0__", "kind": "string", "length": 4},
                {"placeholder": "__SLIPLANE_GENERATED_1__", "kind": "string", "length": 16}
            ],
            "presets": [
                {"name": "PostgreSQL", "tags": ["database"], "description": "A relational database.",
                 "volumes": [{"id": "pg-__SLIPLANE_GENERATED_0__", "name": "pg-data-__SLIPLANE_GENERATED_0__"}],
                 "settings": {"presetId": "postgres", "name": "Postgres-__SLIPLANE_GENERATED_0__",
                   "envs": [{"key": "POSTGRES_PASSWORD", "value": "__SLIPLANE_GENERATED_1__", "secret": false},
                            {"key": "POSTGRES_DB", "value": "mydb", "secret": false}],
                   "public": true, "protocol": "tcp", "deploySource": "registry",
                   "externalDockerImage": "docker.io/library/postgres:18.6", "cmd": "",
                   "volumes": [{"id": "pg-__SLIPLANE_GENERATED_0__", "containerMountPath": "/var/lib/postgresql"}]}},
                {"name": "n8n", "tags": ["automation"], "description": "Workflow automation.",
                 "volumes": [], "settings": {"presetId": "n8n", "name": "n8n-__SLIPLANE_GENERATED_0__",
                   "healthcheckPath": "/", "envs": [{"key": "OPENAI_API_KEY", "value": "", "secret": true}],
                   "public": true, "protocol": "http", "deploySource": "registry",
                   "externalDockerImage": "docker.io/n8nio/n8n:2.35.7", "cmd": "", "volumes": []}}
            ]
        }),
    )
}

#[test]
fn presets_list_and_show_need_no_account_and_never_send_a_key() {
    let mock = Mock::start(vec![catalog()]);
    let env = Env::new(&mock);

    let (code, out, err) = env.json(&["presets", "list"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.as_array().unwrap().len(), 2);
    assert_eq!(out[0]["id"], "postgres");
    assert_eq!(out[0]["image"], "docker.io/library/postgres:18.6");

    let (_, out, _) = env.json(&["presets", "list", "--search", "autom"]);
    assert_eq!(out.as_array().unwrap().len(), 1);
    assert_eq!(out[0]["id"], "n8n");
    let (_, out, _) = env.json(&["presets", "list", "--tag", "database"]);
    assert_eq!(out[0]["id"], "postgres");

    let (code, out, _) = env.json(&["presets", "show", "n8n"]);
    assert_eq!(code, 0);
    assert_eq!(out["env"][0]["key"], "OPENAI_API_KEY");
    assert_eq!(out["env"][0]["required"], true);

    let (code, _, err) = env.json(&["presets", "show", "nope"]);
    assert_eq!(code, 4);
    assert_eq!(err["remediation"], "sliplane presets list");

    for r in mock.requests() {
        assert!(!r.headers.iter().any(|(k, _)| k == "authorization"), "a key was sent to the catalog: {r:?}");
    }
}

#[test]
fn presets_deploy_needs_an_account_before_it_does_anything() {
    let mock = Mock::start(vec![me(), catalog()]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["presets", "deploy", "postgres", "--project-id", "p1", "--server-id", "srv1"]);
    assert_eq!(code, 7);
    assert_eq!(err["code"], "no_account");
    assert!(!mock.requests().iter().any(|r| r.path == "presets"), "the catalog was fetched without an account");
}

#[test]
fn presets_dry_run_creates_nothing_and_hides_secrets() {
    let mock = Mock::start(vec![me(), catalog()]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&[
        "presets",
        "deploy",
        "postgres",
        "--project-id",
        "p1",
        "--server-id",
        "srv1",
        "--dry-run",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["dryRun"], true);
    let password = out["request"]["env"].as_array().unwrap().iter().find(|e| e["key"] == "POSTGRES_PASSWORD").unwrap();
    assert_eq!(password["value"], "<hidden>");
    assert!(out["warnings"][0].as_str().unwrap().contains("--private"));
    assert!(!mock.requests().iter().any(|r| r.method == "POST"), "a dry run must not create anything");
}

#[test]
fn presets_deploy_creates_the_service_and_prints_the_generated_secrets_once() {
    let mock =
        Mock::start(vec![me(), catalog(), ("POST", "projects/p1/services", 201, json!({"id": "svc1", "name": "db"}))]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&[
        "presets",
        "deploy",
        "postgres",
        "--project-id",
        "p1",
        "--server-id",
        "srv1",
        "--name",
        "db",
        "--private",
        "--env",
        "POSTGRES_DB=orders",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");

    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["name"], "db");
    assert_eq!(body["serverId"], "srv1");
    assert_eq!(body["deployment"], json!({"url": "docker.io/library/postgres:18.6"}));
    assert_eq!(body["network"], json!({"public": false}), "--private must not expose the database");
    let env_of = |k: &str| body["env"].as_array().unwrap().iter().find(|e| e["key"] == k).unwrap().clone();
    assert_eq!(env_of("POSTGRES_DB")["value"], "orders", "--env overrides the preset's value");
    assert_eq!(body["volumes"][0]["mountPath"], "/var/lib/postgresql");
    assert!(body["volumes"][0]["name"].as_str().unwrap().starts_with("pg-data-"));

    // The password that was sent is the one printed, because it can never be read back later.
    assert_eq!(out["service"]["id"], "svc1");
    assert_eq!(out["generated"][0]["key"], "POSTGRES_PASSWORD");
    assert_eq!(out["generated"][0]["value"], env_of("POSTGRES_PASSWORD")["value"]);
    assert_eq!(out["generated"][0]["value"].as_str().unwrap().len(), 16);
    assert!(out["warnings"].is_null(), "private, so nothing to warn about: {out}");
}

#[test]
fn presets_deploy_refuses_an_empty_required_value_without_creating_anything() {
    let mock = Mock::start(vec![me(), catalog()]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["presets", "deploy", "n8n", "--project-id", "p1", "--server-id", "srv1", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["remediation"].as_str().unwrap().contains("--secret-env OPENAI_API_KEY=<value>"));
    assert!(!mock.requests().iter().any(|r| r.method == "POST"));
}

fn event(id: &str, kind: &str, message: &str) -> Value {
    json!({"id": id, "type": kind, "message": message, "createdAt": "2026-10-31T12:00:00Z"})
}

#[test]
fn presets_deploy_wait_reports_the_outcome() {
    let mock = Mock::start(vec![
        me(),
        catalog(),
        ("POST", "projects/p1/services", 201, json!({"id": "svc1"})),
        (
            "GET",
            "projects/p1/services/svc1/events",
            200,
            json!([event("e1", "service_deploy_success", "Deploy succeeded")]),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let args = ["presets", "deploy", "postgres", "--project-id", "p1", "--server-id", "srv1", "--wait", "-a", "work"];
    let (code, out, err) = env.json(&args);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["deploy"]["status"], "live");
    assert_eq!(out["deploy"]["event"]["id"], "e1");
}

#[test]
fn a_failed_wait_still_prints_the_secrets_it_generated() {
    let mock = Mock::start(vec![
        me(),
        catalog(),
        ("POST", "projects/p1/services", 201, json!({"id": "svc1"})),
        (
            "GET",
            "projects/p1/services/svc1/events",
            200,
            json!([event("e1", "service_deploy_failed", "image pull failed")]),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let args = ["presets", "deploy", "postgres", "--project-id", "p1", "--server-id", "srv1", "--wait", "-a", "work"];
    let (code, out, err) = env.json(&args);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("image pull failed"), "{err}");
    // The service exists, and its password is unrecoverable unless it was printed.
    assert_eq!(out["service"]["id"], "svc1");
    assert_eq!(out["generated"][0]["value"].as_str().unwrap().len(), 16);
    assert_eq!(out["deploy"]["status"], "not_confirmed");
}

// ---------------------------------------------------------------------------------------------
// --wait on services

const EVENTS: &str = "projects/p1/services/s1/events";

fn deploy_wait(env: &Env, extra: &[&str]) -> (i32, Value, Value) {
    let mut args = vec!["services", "deploy", "--project-id", "p1", "--service-id", "s1", "--wait", "-a", "work"];
    args.extend_from_slice(extra);
    env.json(&args)
}

#[test]
fn deploy_wait_ignores_old_events_and_reports_the_new_outcome() {
    let old = event("e0", "service_deploy_success", "an earlier deploy");
    let new = event("e1", "service_deploy_success", "this deploy");
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!({"__seq": [[old.clone()], [old, new]]})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = deploy_wait(&env, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "live");
    assert_eq!(out["action"], "deploy");
    assert_eq!(out["event"]["id"], "e1", "the earlier deploy's success must not count");

    // The baseline is read before the deploy is triggered, or the new events would be ignored too.
    let order: Vec<String> = mock.requests().iter().map(|r| format!("{} {}", r.method, r.path)).collect();
    let post = order.iter().position(|r| r.ends_with("/deploy")).unwrap();
    assert!(order[..post].iter().any(|r| r.ends_with("/events")), "{order:?}");
}

#[test]
fn deploy_wait_polls_until_there_is_an_outcome() {
    let old = event("e0", "service_deploy_success", "an earlier deploy");
    let new = event("e1", "service_deploy_success", "this deploy");
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!({"__seq": [[old.clone()], [old.clone()], [old, new]]})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = deploy_wait(&env, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["event"]["id"], "e1");
    assert!(mock.requests().iter().filter(|r| r.path == EVENTS).count() >= 3);
}

#[test]
fn deploy_wait_fails_when_the_deploy_does() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!({"__seq": [[], [event("e1", "service_build_failed", "Dockerfile not found")]]})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = deploy_wait(&env, &[]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("Dockerfile not found"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("services logs"));
}

#[test]
fn deploy_wait_gives_up_at_the_timeout() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!([event("e0", "service_deploy_success", "old")])),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = deploy_wait(&env, &["--timeout", "1"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("No deploy outcome after 1 seconds"), "{err}");
}

#[test]
fn timeout_without_wait_is_refused() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let (code, _, _) =
        env.json(&["services", "deploy", "--project-id", "p1", "--service-id", "s1", "--timeout", "5", "-a", "work"]);
    assert_eq!(code, 6);
}

#[test]
fn create_wait_reports_the_deploy_on_the_service() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services", 201, json!({"id": "s9", "name": "web"})),
        ("GET", "projects/p1/services/s9/events", 200, json!([event("e1", "service_deploy_success", "ok")])),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&[
        "services",
        "create",
        "--project-id",
        "p1",
        "--name",
        "web",
        "--server-id",
        "srv1",
        "--image",
        "nginx",
        "--wait",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s9");
    assert_eq!(out["deploy"]["status"], "live");
}

// ---------------------------------------------------------------------------------------------
// logs

const LOGS: &str = "projects/p1/services/s1/logs";
const LOGS_FROM_EPOCH_2000: &str = "projects/p1/services/s1/logs?from=951868800";
const LOGS_FROM_LATEST: &str = "projects/p1/services/s1/logs?from=951868801";

fn log_line(at: &str, message: &str) -> Value {
    json!({"createdAt": at, "message": message})
}

#[test]
fn logs_since_becomes_from() {
    let mock = Mock::start(vec![me(), ("GET", LOGS_FROM_EPOCH_2000, 200, json!([]))]);
    let env = Env::new(&mock).with_account();
    let base = ["services", "logs", "--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        env.json(&args)
    };

    let (code, _, err) = with(&["--since", "2000-03-01T00:00:00Z"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("GET").path, LOGS_FROM_EPOCH_2000);

    let (code, _, err) = with(&["--since", "yesterday"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("neither a duration"));
}

#[test]
fn logs_follow_prints_each_line_once_oldest_first() {
    let (a, b, c) = (
        log_line("2000-03-01T00:00:01Z", "a"),
        log_line("2000-03-01T00:00:02Z", "b"),
        log_line("2000-03-01T00:00:03Z", "c"),
    );
    let mock = Mock::start(vec![
        me(),
        // Newest first, as the API sends them.
        ("GET", LOGS, 200, json!([b.clone(), a])),
        // The next poll starts one second before the newest line seen, so "b" comes back.
        ("GET", LOGS_FROM_LATEST, 200, json!([c, b])),
    ]);
    let env = Env::new(&mock).with_account();
    let out = env.run(&[
        "services",
        "logs",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--follow",
        "--interval",
        "1",
        "--for",
        "1s",
        "--json",
        "-a",
        "work",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with("warning:"))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let messages: Vec<&str> = lines.iter().map(|l| l["message"].as_str().unwrap()).collect();
    assert_eq!(messages, ["a", "b", "c"]);

    let polls: Vec<String> =
        mock.requests().iter().filter(|r| r.path.starts_with(LOGS)).map(|r| r.path.clone()).collect();
    assert_eq!(polls[0], LOGS);
    assert_eq!(polls[1], LOGS_FROM_LATEST);
}

#[test]
fn logs_follow_flags_are_checked() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let base = ["services", "logs", "--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        env.json(&args)
    };
    assert_eq!(with(&["--follow", "--to", "5"]).0, 6, "--to pages backwards; following goes forwards");
    assert_eq!(with(&["--interval", "3"]).0, 6, "--interval needs --follow");
    assert_eq!(with(&["--for", "1m"]).0, 6, "--for needs --follow");
    assert_eq!(with(&["--since", "5m", "--from", "100"]).0, 6);
    let (code, _, err) = with(&["--follow", "--for", "5x"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("not a duration"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// version, completion

#[test]
fn version_reports_the_build_and_the_api_it_targets() {
    let mock = Mock::start(vec![]);
    let (code, out, _) = Env::new(&mock).json(&["version"]);
    assert_eq!(code, 0);
    assert_eq!(out["name"], "sliplane");
    assert_eq!(out["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(out["apiSpec"], "0.11.0");
}

#[test]
fn completion_prints_a_script_for_each_shell() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    for (shell, marker) in [
        ("bash", "complete -F"),
        ("zsh", "#compdef sliplane"),
        ("fish", "complete -c sliplane"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let out = env.run(&["completion", shell]);
        assert_eq!(out.status.code(), Some(0), "{shell}");
        let script = String::from_utf8_lossy(&out.stdout);
        assert!(script.contains(marker), "{shell}: {}", &script[..script.len().min(200)]);
        assert!(script.contains("presets"), "{shell} completions should know the new commands");
    }
    assert_eq!(env.run(&["completion", "tcsh"]).status.code(), Some(6));
}

// ---------------------------------------------------------------------------------------------
// names for ids

fn two_projects() -> Vec<Route> {
    vec![
        ("GET", "projects", 200, json!([{"id": "p1", "name": "Main"}, {"id": "p2", "name": "Lab"}])),
        (
            "GET",
            "projects/p1/services",
            200,
            json!([{"id": "s1", "name": "web", "projectId": "p1"}, {"id": "s2", "name": "worker", "projectId": "p1"}]),
        ),
        ("GET", "projects/p2/services", 200, json!([{"id": "s3", "name": "web", "projectId": "p2"}])),
        ("GET", "projects/p1/services/s1", 200, json!({"id": "s1", "name": "web"})),
        ("GET", "projects/p1/services/s2", 200, json!({"id": "s2", "name": "worker"})),
        ("GET", "projects/p2/services/s3", 200, json!({"id": "s3", "name": "web"})),
    ]
}

fn mock_with(extra: Vec<Route>) -> Mock {
    let mut routes = vec![me()];
    routes.extend(extra);
    Mock::start(routes)
}

fn paths(mock: &Mock, method: &str) -> Vec<String> {
    mock.requests().into_iter().filter(|r| r.method == method).map(|r| r.path).collect()
}

#[test]
fn ids_alone_make_no_lookup_requests() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let before = mock.requests().len();
    let (code, _, err) = env.json(&["services", "get", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    let made: Vec<String> = mock.requests()[before..].iter().map(|r| r.path.clone()).collect();
    assert_eq!(made, ["projects/p1/services/s1"], "an id-only command must cost exactly one request");
}

#[test]
fn a_service_is_found_by_name_within_a_named_project() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "get", "--project", "Main", "--service", "web", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s1");
    assert_eq!(mock.last("GET").path, "projects/p1/services/s1");
    let gets = paths(&mock, "GET");
    assert!(gets.contains(&"projects".to_string()) && gets.contains(&"projects/p1/services".to_string()), "{gets:?}");
}

#[test]
fn a_unique_name_is_found_without_naming_the_project() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "get", "--service", "worker", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s2");
    assert_eq!(mock.last("GET").path, "projects/p1/services/s2");
}

#[test]
fn an_id_alone_finds_its_project() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "get", "--service-id", "s3", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s3");
    assert_eq!(mock.last("GET").path, "projects/p2/services/s3");

    let (code, _, err) = env.json(&["services", "get", "--service-id", "nope", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["error"].as_str().unwrap().contains("'nope'"), "{err}");
}

#[test]
fn a_name_two_services_share_is_refused_with_both_ids() {
    let mut routes = two_projects();
    routes.push(("DELETE", "projects/p2/services/s3", 204, json!(null)));
    let mock = mock_with(routes);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["services", "delete", "--service", "web", "-a", "work"]);
    assert_eq!(code, 6, "{err}");
    let detail = err["detail"].as_str().unwrap();
    assert!(detail.contains("s1 (project p1)") && detail.contains("s3 (project p2)"), "{detail}");
    assert!(err["remediation"].as_str().unwrap().contains("--project"), "{err}");
    assert!(paths(&mock, "DELETE").is_empty(), "nothing may be deleted when the name is ambiguous");

    // Naming the project makes it unambiguous.
    let (code, _, err) = env.json(&["services", "delete", "--project", "Lab", "--service", "web", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("DELETE").path, "projects/p2/services/s3");
}

#[test]
fn an_unknown_name_is_not_found_and_shows_what_exists() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["services", "get", "--project", "Main", "--service", "nope", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["detail"].as_str().unwrap().contains("worker (s2)"), "{err}");
    assert_eq!(err["remediation"], "sliplane services list --account <account>");

    let (code, _, err) = env.json(&["services", "get", "--project", "Nowhere", "--service", "web", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["error"].as_str().unwrap().contains("No project"), "{err}");
}

#[test]
fn an_id_and_a_name_for_the_same_thing_conflict_and_neither_is_refused_early() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock);
    // No account exists here: both must fail at argument checking, before any keystore or network use.
    let (code, _, err) = env.json(&["services", "get", "--service-id", "s1", "--service", "web", "-a", "work"]);
    assert_eq!(code, 6, "{err}");
    let (code, _, err) = env.json(&["services", "get", "--project-id", "p1", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("--service-id"), "{err}");
    assert!(mock.requests().is_empty());
}

#[test]
fn servers_databases_and_buckets_take_names() {
    let mock = mock_with(vec![
        ("GET", "servers", 200, json!([{"id": "server_1", "name": "big"}])),
        ("GET", "servers/server_1", 200, json!({"id": "server_1"})),
        ("GET", "postgres", 200, json!([{"id": "pg_1", "name": "orders"}])),
        ("GET", "postgres/pg_1", 200, json!({"id": "pg_1"})),
        ("GET", "buckets", 200, json!([{"id": "bucket_1", "name": "assets"}])),
        ("GET", "buckets/bucket_1/keys", 200, json!([])),
    ]);
    let env = Env::new(&mock).with_account();
    assert_eq!(env.json(&["servers", "get", "--server", "big", "-a", "work"]).0, 0);
    assert_eq!(mock.last("GET").path, "servers/server_1");
    assert_eq!(env.json(&["postgres", "get", "--postgres", "orders", "-a", "work"]).0, 0);
    assert_eq!(mock.last("GET").path, "postgres/pg_1");
    assert_eq!(env.json(&["buckets", "keys", "--bucket", "assets", "-a", "work"]).0, 0);
    assert_eq!(mock.last("GET").path, "buckets/bucket_1/keys");
    let (code, _, err) = env.json(&["postgres", "get", "--postgres", "nope", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["error"].as_str().unwrap().contains("Postgres database"), "{err}");
}

#[test]
fn projects_are_renamed_and_deleted_by_name() {
    let mock = mock_with(
        two_projects()
            .into_iter()
            .take(1)
            .chain([
                ("PATCH", "projects/p1", 200, json!({"id": "p1", "name": "Primary"})),
                ("DELETE", "projects/p2", 204, json!(null)),
            ])
            .collect(),
    );
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["projects", "update", "--project", "Main", "--name", "Primary", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").path, "projects/p1");
    let (code, out, err) = env.json(&["projects", "delete", "--project", "Lab", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["projectId"], "p2");
    assert_eq!(mock.last("DELETE").path, "projects/p2");
}

#[test]
fn create_takes_a_project_and_server_by_name() {
    let mut routes = two_projects().into_iter().take(1).collect::<Vec<_>>();
    routes.push(("GET", "servers", 200, json!([{"id": "server_1", "name": "big"}])));
    routes.push(("POST", "projects/p1/services", 201, json!({"id": "s9"})));
    let mock = mock_with(routes);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "create",
        "--project",
        "Main",
        "--name",
        "api",
        "--server",
        "big",
        "--image",
        "nginx",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["serverId"], "server_1");
    assert_eq!(mock.last("POST").path, "projects/p1/services");
}

#[test]
fn services_list_takes_a_project_by_name() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "list", "--project", "Lab", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out[0]["id"], "s3");
    assert_eq!(mock.last("GET").path, "projects/p2/services");
}

// ---------------------------------------------------------------------------------------------
// postgres allowlist, logs --since, and restore's default point

const PG_PASSWORD: &str = "pg-secret-pw";

fn pg_db(list: Value) -> Value {
    json!({
        "id": "pg_1", "name": "main", "status": "running", "ipAllowList": list,
        "connection": {"host": "pg.example", "port": 5432, "database": "d", "user": "u", "password": PG_PASSWORD,
                       "uri": format!("postgres://u:{PG_PASSWORD}@pg.example:5432/d")},
    })
}

fn entry(id: &str, source: &str, description: &str) -> Value {
    let mut e = json!({"id": id, "source": source, "createdAt": "2026-10-01T00:00:00Z"});
    if !description.is_empty() {
        e["description"] = json!(description);
    }
    e
}

/// `me`, which also reports the caller's address in the `Sliplane-Client-IP` header.
fn me_ip(ip: &str) -> Route {
    (
        "GET",
        "me",
        200,
        json!({"__headers": {"Sliplane-Client-IP": ip},
               "__body": {"authType": "apikey", "tokenType": "rw", "organizationId": "org_1",
                          "user": {"email": "dev@example.com"}}}),
    )
}

fn allowlist(env: &Env, args: &[&str]) -> (i32, Value, Value) {
    let mut all = vec!["postgres", "allowlist"];
    all.extend_from_slice(args);
    all.extend(["--postgres-id", "pg_1", "-a", "work"]);
    env.json(&all)
}

fn patches(mock: &Mock) -> Vec<Recorded> {
    mock.requests().into_iter().filter(|r| r.method == "PATCH").collect()
}

fn sources(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|e| e["source"].as_str().unwrap().to_string()).collect()
}

fn count(mock: &Mock, path: &str) -> usize {
    paths(mock, "GET").iter().filter(|p| *p == path).count()
}

#[test]
fn allowlist_show_says_what_the_list_means_and_reads_the_callers_address() {
    let list = json!([entry("pgfw_1", "10.0.0.0/8", "vpn"), entry("pgfw_2", "203.0.113.7/32", "")]);
    let mock = Mock::start(vec![me_ip("203.0.113.7"), ("GET", "postgres/pg_1", 200, pg_db(list.clone()))]);
    let env = Env::new(&mock).with_account();

    let raw = env.run(&["postgres", "allowlist", "show", "--postgres-id", "pg_1", "-a", "work", "--json"]);
    let text = String::from_utf8(raw.stdout).unwrap();
    assert!(!text.contains(PG_PASSWORD), "the allow list output must not carry the connection password: {text}");
    let (code, out, err) = allowlist(&env, &["show"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["postgresId"], "pg_1");
    assert_eq!(out["name"], "main");
    assert_eq!(out["access"], "restricted");
    assert_eq!(out["ipAllowList"], list);
    assert!(out.get("myIp").is_none() && out.get("warnings").is_none() && out.get("status").is_none());

    // --my-ip: the address comes from the Sliplane-Client-IP header of `me`, and the key goes with that request.
    let before = count(&mock, "me");
    let (code, out, err) = allowlist(&env, &["show", "--my-ip"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["myIp"], "203.0.113.7");
    assert_eq!(out["myIpAllowed"], true);
    assert_eq!(count(&mock, "me"), before + 1);
    let probe = mock.requests().into_iter().rev().find(|r| r.path == "me").unwrap();
    assert_eq!(probe.headers.iter().find(|(k, _)| k == "authorization").unwrap().1, "Bearer sl_test");
    assert!(patches(&mock).is_empty(), "show changes nothing");

    // An address the list does not cover.
    let mock = Mock::start(vec![me_ip("198.51.100.9"), ("GET", "postgres/pg_1", 200, pg_db(list))]);
    let env = Env::new(&mock).with_account();
    let (_, out, _) = allowlist(&env, &["show", "--my-ip"]);
    assert_eq!(out["myIp"], "198.51.100.9");
    assert_eq!(out["myIpAllowed"], false);
}

#[test]
fn allowlist_show_flags_open_and_blocked_databases() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_1", "0.0.0.0/0", ""), entry("pgfw_2", "::/0", "")]))),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, _) = allowlist(&env, &["show"]);
    assert_eq!(code, 0);
    assert_eq!(out["access"], "open");
    let warnings = out["warnings"].as_array().unwrap();
    assert!(warnings[0].as_str().unwrap().contains("every IPv4 address"), "{warnings:?}");
    assert!(warnings[1].as_str().unwrap().contains("every IPv6 address"), "{warnings:?}");

    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1", 200, pg_db(json!([])))]);
    let env = Env::new(&mock).with_account();
    let (_, out, _) = allowlist(&env, &["show"]);
    assert_eq!(out["access"], "blocked");
    assert!(out["warnings"][0].as_str().unwrap().contains("refuses every public connection"));
}

#[test]
fn allowlist_add_keeps_unrelated_entries_and_appends_without_duplicates() {
    let before = json!([entry("pgfw_1", "10.0.0.0/8", "vpn"), entry("pgfw_2", "192.0.2.5/24", "")]);
    let after = json!([
        entry("pgfw_1", "10.0.0.0/8", "vpn"),
        entry("pgfw_2", "192.0.2.5/24", ""),
        entry("pgfw_3", "203.0.113.7/32", ""),
        entry("pgfw_4", "198.51.100.0/24", ""),
    ]);
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1", 200, pg_db(before)),
        ("PATCH", "postgres/pg_1", 200, pg_db(after.clone())),
    ]);
    let env = Env::new(&mock).with_account();

    // 10.9.9.9/8 is the vpn range again, and 203.0.113.7 twice is once; a bare address is a host range.
    let (code, out, err) = allowlist(&env, &["add", "203.0.113.7", "198.51.100.77/24", "203.0.113.7/32", "10.9.9.9/8"]);
    assert_eq!(code, 0, "{err}");
    let sent = patches(&mock);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].path, "postgres/pg_1");
    assert_eq!(
        sent[0].body.clone().unwrap(),
        json!({"ipAllowList": [
            {"source": "10.0.0.0/8", "description": "vpn"},
            {"source": "192.0.2.5/24"},
            {"source": "203.0.113.7/32"},
            {"source": "198.51.100.0/24"},
        ]}),
        "the whole list goes back, in order, with what was there untouched and the new ones last"
    );
    assert_eq!(out["status"], "updated");
    assert_eq!(out["added"], json!(["203.0.113.7/32", "198.51.100.0/24"]));
    assert_eq!(out["ipAllowList"], after, "the list printed is the one the API answered with");
    assert_eq!(out["access"], "restricted");
}

#[test]
fn allowlist_add_labels_and_does_nothing_when_there_is_nothing_to_do() {
    let list = json!([entry("pgfw_1", "10.0.0.0/8", "vpn")]);
    let relabeled = json!([entry("pgfw_1", "10.0.0.0/8", "office"), entry("pgfw_9", "192.0.2.1/32", "office")]);
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1", 200, pg_db(list.clone())),
        ("PATCH", "postgres/pg_1", 200, pg_db(relabeled)),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, out, err) = allowlist(&env, &["add", "10.1.1.1/8"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "unchanged");
    assert_eq!(out["ipAllowList"], list);
    assert!(patches(&mock).is_empty(), "an entry that is already there is not written again");

    let (code, out, err) = allowlist(&env, &["add", "10.0.0.0/8", "192.0.2.1", "--description", "office"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        patches(&mock)[0].body.clone().unwrap(),
        json!({"ipAllowList": [
            {"source": "10.0.0.0/8", "description": "office"},
            {"source": "192.0.2.1/32", "description": "office"},
        ]})
    );
    assert_eq!(out["added"], json!(["192.0.2.1/32"]));
    assert_eq!(out["relabeled"], json!(["10.0.0.0/8"]));
}

#[test]
fn allowlist_add_needs_something_to_add_and_valid_addresses() {
    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1", 200, pg_db(json!([])))]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = allowlist(&env, &["add"]);
    assert_eq!((code, err["code"].as_str()), (6, Some("invalid_input")));
    for bad in ["203.0.113.256", "10.0.0.0/33", "example.com", "10.0.0.0/"] {
        let (code, _, err) = allowlist(&env, &["add", "203.0.113.7", bad]);
        assert_eq!(code, 6, "{bad}");
        assert!(err["error"].as_str().unwrap().contains(bad), "{err}");
    }
    assert_eq!(count(&mock, "postgres/pg_1"), 0, "nothing is read for input that cannot be valid");
    assert!(patches(&mock).is_empty());
}

#[test]
fn allowlist_add_refuses_to_open_the_database_to_the_world_without_allow_open() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_1", "10.0.0.0/8", "")]))),
        (
            "PATCH",
            "postgres/pg_1",
            200,
            pg_db(json!([entry("pgfw_1", "10.0.0.0/8", ""), entry("pgfw_2", "0.0.0.0/0", "")])),
        ),
    ]);
    let env = Env::new(&mock).with_account();

    // However it is written, a /0 is the whole internet; the refusal comes before anything is read or written.
    for open in ["0.0.0.0/0", "::/0", "1.2.3.4/0", "::0/0"] {
        let (code, _, err) = allowlist(&env, &["add", "203.0.113.7", open]);
        assert_eq!((code, err["code"].as_str()), (6, Some("invalid_input")), "{open}");
        assert!(err["error"].as_str().unwrap().contains("whole internet"), "{err}");
        assert!(err["remediation"].as_str().unwrap().contains("--allow-open"), "{err}");
    }
    assert_eq!(count(&mock, "postgres/pg_1"), 0);
    assert!(patches(&mock).is_empty());

    let (code, out, err) = allowlist(&env, &["add", "0.0.0.0/0", "--allow-open"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        patches(&mock)[0].body.clone().unwrap(),
        json!({"ipAllowList": [{"source": "10.0.0.0/8"}, {"source": "0.0.0.0/0"}]})
    );
    assert_eq!(out["access"], "open");
    assert!(out["warnings"][0].as_str().unwrap().contains("whole internet"), "{out}");
}

#[test]
fn allowlist_add_my_ip_follows_the_lists_family_and_says_so() {
    let mock = Mock::start(vec![
        me_ip("203.0.113.7"),
        ("GET", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_1", "10.0.0.0/8", "")]))),
        (
            "PATCH",
            "postgres/pg_1",
            200,
            pg_db(json!([entry("pgfw_1", "10.0.0.0/8", ""), entry("pgfw_2", "203.0.113.7/32", "laptop")])),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let out = env.run(&[
        "postgres",
        "allowlist",
        "add",
        "--my-ip",
        "--description",
        "laptop",
        "--postgres-id",
        "pg_1",
        "-a",
        "work",
        "--json",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("note: --my-ip is the IPv4 address, because every entry on the allow list is IPv4"),
        "{stderr}"
    );
    assert_eq!(
        patches(&mock)[0].body.clone().unwrap(),
        json!({"ipAllowList": [{"source": "10.0.0.0/8"}, {"source": "203.0.113.7/32", "description": "laptop"}]})
    );
}

#[test]
fn allowlist_my_ip_needs_a_family_when_the_list_gives_none() {
    for list in [json!([]), json!([entry("pgfw_1", "10.0.0.0/8", ""), entry("pgfw_2", "2001:db8::/32", "")])] {
        let mock = Mock::start(vec![
            me_ip("203.0.113.7"),
            ("GET", "postgres/pg_1", 200, pg_db(list)),
            ("PATCH", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_9", "203.0.113.7/32", "")]))),
        ]);
        let env = Env::new(&mock).with_account();
        let before = count(&mock, "me");
        let (code, _, err) = allowlist(&env, &["add", "--my-ip"]);
        assert_eq!((code, err["code"].as_str()), (6, Some("invalid_input")));
        assert!(err["remediation"].as_str().unwrap().contains("--ipv4"), "{err}");
        assert!(patches(&mock).is_empty());
        assert_eq!(count(&mock, "me"), before, "no address is guessed at when the family is unknown");

        let (code, _, err) = allowlist(&env, &["add", "--my-ip", "--ipv4"]);
        assert_eq!(code, 0, "{err}");
        let sent = patches(&mock)[0].body.clone().unwrap();
        assert!(sources(&sent["ipAllowList"]).contains(&"203.0.113.7/32".to_string()), "{sent}");
    }
}

#[test]
fn allowlist_family_flags_are_checked_by_the_parser() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    for args in [&["add", "10.0.0.1", "--ipv4"][..], &["add", "--my-ip", "--ipv4", "--ipv6"], &["show", "--ipv6"]] {
        let (code, _, err) = allowlist(&env, args);
        assert_eq!((code, err["code"].as_str()), (6, Some("invalid_input")), "{args:?}");
    }
}

#[test]
fn allowlist_my_ip_fails_clearly_when_the_address_cannot_be_had() {
    let list = pg_db(json!([entry("pgfw_1", "10.0.0.0/8", "")]));
    // The mock listens on IPv4 only, so an IPv6 probe has nothing to connect to: the family really is forced.
    let mock = Mock::start(vec![me_ip("203.0.113.7"), ("GET", "postgres/pg_1", 200, list.clone())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = allowlist(&env, &["add", "--my-ip", "--ipv6"]);
    assert_eq!((code, err["code"].as_str()), (2, Some("network")), "{err}");
    assert!(err["error"].as_str().unwrap().contains("over IPv6"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("--ipv4"), "{err}");
    assert!(patches(&mock).is_empty());

    // An answer in the wrong family, or with no address, is not trusted either.
    for (header, wanted) in [("2001:db8::7", "answered"), ("", "did not report"), ("not-an-ip", "did not report")] {
        let mock = Mock::start(vec![me_ip(header), ("GET", "postgres/pg_1", 200, list.clone())]);
        let env = Env::new(&mock).with_account();
        let (code, _, err) = allowlist(&env, &["add", "--my-ip", "--ipv4"]);
        assert_eq!(code, 1, "{header:?}: {err}");
        assert!(err["error"].as_str().unwrap().contains(wanted), "{header:?}: {err}");
        assert!(patches(&mock).is_empty());
    }
    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1", 200, list)]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = allowlist(&env, &["add", "--my-ip", "--ipv4"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("did not report"), "{err}");
}

#[test]
fn allowlist_remove_takes_ranges_ids_and_this_machine_and_keeps_the_rest() {
    let before = json!([
        entry("pgfw_1", "10.0.0.0/8", "vpn"),
        entry("pgfw_2", "203.0.113.7/32", "me"),
        entry("pgfw_3", "2001:db8::/32", ""),
        entry("pgfw_4", "192.0.2.0/24", "office"),
    ]);
    let after = json!([entry("pgfw_1", "10.0.0.0/8", "vpn")]);
    let mock = Mock::start(vec![
        me_ip("203.0.113.7"),
        ("GET", "postgres/pg_1", 200, pg_db(before)),
        ("PATCH", "postgres/pg_1", 200, pg_db(after.clone())),
    ]);
    let env = Env::new(&mock).with_account();

    // By range, by id, and by --my-ip, together.
    let (code, out, err) = allowlist(&env, &["remove", "192.0.2.0/24", "pgfw_3", "--my-ip", "--ipv4"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        patches(&mock)[0].body.clone().unwrap(),
        json!({"ipAllowList": [{"source": "10.0.0.0/8", "description": "vpn"}]})
    );
    assert_eq!(out["status"], "updated");
    assert_eq!(sources(&out["removed"]), ["203.0.113.7/32", "2001:db8::/32", "192.0.2.0/24"]);
    assert_eq!(out["removed"][0]["description"], "me");
    assert_eq!(out["ipAllowList"], after);

    // A bare address names its host range.
    let (code, _, err) = allowlist(&env, &["remove", "203.0.113.7"]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn allowlist_remove_names_what_it_could_not_find_and_writes_nothing() {
    let mock = Mock::start(vec![
        me(),
        (
            "GET",
            "postgres/pg_1",
            200,
            pg_db(json!([entry("pgfw_1", "10.0.0.0/8", ""), entry("pgfw_2", "192.0.2.1/32", "")])),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = allowlist(&env, &["remove", "192.0.2.1", "10.0.0.0/16", "pgfw_9"]);
    assert_eq!((code, err["code"].as_str()), (4, Some("not_found")));
    let message = err["error"].as_str().unwrap();
    assert!(
        message.contains("10.0.0.0/16") && message.contains("pgfw_9") && !message.contains("192.0.2.1"),
        "{message}"
    );
    assert!(err["detail"].as_str().unwrap().contains("10.0.0.0/8 (pgfw_1)"), "{err}");
    assert!(patches(&mock).is_empty(), "one unmatched target stops the whole removal");
    assert_eq!(allowlist(&env, &["remove"]).0, 6);
}

#[test]
fn allowlist_remove_will_not_empty_the_list_by_accident() {
    let mock = Mock::start(vec![
        me(),
        (
            "GET",
            "postgres/pg_1",
            200,
            pg_db(json!([entry("pgfw_1", "10.0.0.0/8", ""), entry("pgfw_2", "192.0.2.1/32", "")])),
        ),
        ("PATCH", "postgres/pg_1", 200, pg_db(json!([]))),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = allowlist(&env, &["remove", "10.0.0.0/8", "pgfw_2"]);
    assert_eq!((code, err["code"].as_str()), (6, Some("invalid_input")));
    assert!(err["error"].as_str().unwrap().contains("last entry"), "{err}");
    assert_eq!(err["remediation"], "sliplane postgres allowlist clear --postgres-id pg_1 -a work");
    assert!(patches(&mock).is_empty());
}

#[test]
fn allowlist_clear_blocks_everyone_and_keeps_a_record() {
    let before = json!([entry("pgfw_1", "10.0.0.0/8", "vpn"), entry("pgfw_2", "203.0.113.7/32", "")]);
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1", 200, pg_db(before)),
        ("PATCH", "postgres/pg_1", 200, pg_db(json!([]))),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = allowlist(&env, &["clear"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(patches(&mock)[0].body.clone().unwrap(), json!({"ipAllowList": []}));
    assert_eq!(out["status"], "updated");
    assert_eq!(out["access"], "blocked");
    assert_eq!(out["ipAllowList"], json!([]));
    assert!(out["warnings"][0].as_str().unwrap().contains("refuses every public connection"), "{out}");
    assert_eq!(sources(&out["removed"]), ["10.0.0.0/8", "203.0.113.7/32"]);
    assert_eq!(out["removed"][0]["description"], "vpn");
    assert_eq!(out["undo"], "sliplane postgres allowlist add --postgres-id pg_1 10.0.0.0/8 203.0.113.7/32 -a work");

    // Already empty: nothing to write.
    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1", 200, pg_db(json!([])))]);
    let env = Env::new(&mock).with_account();
    let (code, out, _) = allowlist(&env, &["clear"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "unchanged");
    assert_eq!(out["access"], "blocked");
    assert!(patches(&mock).is_empty());
}

#[test]
fn allowlist_never_edits_a_list_it_could_not_see() {
    // Writing back only the new entry would replace, and so delete, whatever the API did not show.
    let mut db = pg_db(json!([]));
    db.as_object_mut().unwrap().remove("ipAllowList");
    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1", 200, db), ("PATCH", "postgres/pg_1", 200, json!({}))]);
    let env = Env::new(&mock).with_account();
    for args in [&["add", "203.0.113.7"][..], &["remove", "203.0.113.7"], &["clear"], &["show"]] {
        let (code, _, err) = allowlist(&env, args);
        assert_eq!(code, 1, "{args:?}: {err}");
        assert!(err["error"].as_str().unwrap().contains("no ipAllowList"), "{err}");
    }
    assert!(patches(&mock).is_empty());
}

#[test]
fn allowlist_takes_the_database_by_name() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres", 200, json!([{"id": "pg_1", "name": "main"}])),
        ("GET", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_1", "10.0.0.0/8", "")]))),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["postgres", "allowlist", "show", "--postgres", "main", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["postgresId"], "pg_1");
    assert_eq!(env.json(&["postgres", "allowlist", "show", "-a", "work"]).0, 6, "a database has to be named");
}

/// Seconds since the epoch for `2026-10-31T12:00:00Z`.
fn epoch(ts: &str) -> i64 {
    let n = |a: usize, b: usize| ts[a..b].parse::<i64>().unwrap();
    let (y, m, d) = (n(0, 4), n(5, 7), n(8, 10));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    days * 86_400 + n(11, 13) * 3600 + n(14, 16) * 60 + n(17, 19)
}

#[test]
fn postgres_logs_since_takes_a_duration_or_a_timestamp() {
    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1/logs*", 200, json!([{"message": "ready"}]))]);
    let env = Env::new(&mock).with_account();
    let logs = |args: &[&str]| {
        let mut all = vec!["postgres", "logs", "--postgres-id", "pg_1", "-a", "work"];
        all.extend_from_slice(args);
        env.json(&all)
    };

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let (code, out, err) = logs(&["--since", "30m"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out[0]["message"], "ready");
    let path = mock.last("GET").path;
    let since = path.strip_prefix("postgres/pg_1/logs?since=").unwrap_or_else(|| panic!("{path}")).replace("%3A", ":");
    assert!((epoch(&since) - (now - 1800)).abs() <= 10, "30m ago is {since}, now is {now}");

    // A timestamp is passed on as the instant it names, in UTC.
    let (code, _, err) =
        logs(&["--since", "2026-10-31T12:00:00+02:00", "--until", "2026-10-31T11:00:00Z", "--limit", "5"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("GET").path,
        "postgres/pg_1/logs?since=2026-10-31T10%3A00%3A00Z&until=2026-10-31T11%3A00%3A00Z&limit=5"
    );
    assert_eq!(logs(&[]).0, 0);
    assert_eq!(mock.last("GET").path, "postgres/pg_1/logs", "no flags, no query");

    let requests = mock.requests().len();
    let (code, _, err) = logs(&["--since", "yesterday"]);
    assert_eq!((code, err["code"].as_str()), (6, Some("invalid_input")));
    assert!(err["error"].as_str().unwrap().contains("neither a duration"), "{err}");
    assert_eq!(mock.requests().len(), requests, "a bad --since is refused before anything is requested");
}

#[test]
fn postgres_restore_defaults_to_the_latest_restorable_point() {
    let window = json!({"min": "2026-10-30T10:00:00Z", "max": "2026-10-31T09:59:30Z"});
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1/restore", 200, window),
        ("POST", "postgres/pg_1/restore", 202, json!({"id": "pg_2", "name": "main-restore"})),
    ]);
    let env = Env::new(&mock).with_account();

    let out = env.run(&["postgres", "restore", "--postgres-id", "pg_1", "-a", "work", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(mock.last("POST").body.unwrap(), json!({"timestamp": "2026-10-31T09:59:30Z"}));
    assert!(stderr.contains("latest restorable point, 2026-10-31T09:59:30Z"), "{stderr}");
    assert_eq!(count(&mock, "postgres/pg_1/restore"), 1);

    // An explicit point is used as given, without asking for the window.
    let (code, out, err) = env.json(&[
        "postgres",
        "restore",
        "--postgres-id",
        "pg_1",
        "--timestamp",
        "2026-10-30T12:00:00Z",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("POST").body.unwrap(), json!({"timestamp": "2026-10-30T12:00:00Z"}));
    assert_eq!(out["id"], "pg_2");
    assert_eq!(count(&mock, "postgres/pg_1/restore"), 1);

    // A window with no latest point is an error, not a guess.
    let mock = Mock::start(vec![
        me(),
        ("GET", "postgres/pg_1/restore", 200, json!({})),
        ("POST", "postgres/pg_1/restore", 202, json!({})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["postgres", "restore", "--postgres-id", "pg_1", "-a", "work"]);
    assert_eq!(code, 1, "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("--timestamp"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "POST"));
}

#[test]
fn the_probe_connects_over_the_family_it_was_asked_for() {
    // `[::1]` is a literal address, so the only thing that can make `--ipv4` fail against it is the
    // family filter. Where the machine has no IPv6 loopback (some CI containers) there is no mock to run.
    if TcpListener::bind("[::1]:0").is_err() {
        eprintln!("no IPv6 loopback here; skipping");
        return;
    }
    let routes = |ip: &str| -> Vec<Route> {
        vec![me_ip(ip), ("GET", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_1", "2001:db8::/32", "")])))]
    };
    let v6 = Mock::start_on("[::1]:0", routes("2001:db8::7"));
    let env = Env::new(&v6).with_account();

    let (code, out, err) = allowlist(&env, &["show", "--my-ip", "--ipv6"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["myIp"], "2001:db8::7");
    assert_eq!(out["myIpAllowed"], true);

    // The family of the list is followed when nothing is said.
    let raw = env.run(&["postgres", "allowlist", "show", "--my-ip", "--postgres-id", "pg_1", "-a", "work", "--json"]);
    assert_eq!(raw.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&raw.stderr).contains("note: --my-ip is the IPv6 address"));

    let (code, _, err) = allowlist(&env, &["show", "--my-ip", "--ipv4"]);
    assert_eq!(
        (code, err["code"].as_str()),
        (2, Some("network")),
        "an IPv4 probe cannot reach an IPv6-only server: {err}"
    );
    assert!(err["error"].as_str().unwrap().contains("over IPv4"), "{err}");

    // And the other way round: an IPv4-only server is out of reach of an IPv6 probe.
    let v4 = Mock::start(routes("203.0.113.7"));
    let env = Env::new(&v4).with_account();
    assert_eq!(allowlist(&env, &["show", "--my-ip", "--ipv4"]).0, 0);
    assert_eq!(allowlist(&env, &["show", "--my-ip", "--ipv6"]).0, 2);
}

/// An HTTP CONNECT proxy that tunnels to wherever it is asked and counts the tunnels.
fn connect_proxy(hits: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut client) = client else { continue };
            hits.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(client.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                }
                let Ok(upstream) = std::net::TcpStream::connect(&target) else { return };
                write!(client, "HTTP/1.1 200 Connection established\r\n\r\n").unwrap();
                let (mut from_upstream, mut to_upstream) = (upstream.try_clone().unwrap(), upstream);
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut from_upstream, &mut client);
                });
                let _ = std::io::copy(&mut reader, &mut to_upstream);
            });
        }
    });
    url
}

#[test]
fn the_probe_goes_direct_while_other_requests_use_the_proxy() {
    let hits = Arc::new(AtomicUsize::new(0));
    let proxy = connect_proxy(hits.clone());
    let mock = Mock::start(vec![
        me_ip("203.0.113.7"),
        ("GET", "postgres/pg_1", 200, pg_db(json!([entry("pgfw_1", "203.0.113.0/24", "")]))),
    ]);
    let env = Env::new(&mock).with_account();
    let via_proxy: Vec<(&str, &str)> = ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy", "ALL_PROXY"]
        .into_iter()
        .map(|k| (k, proxy.as_str()))
        .collect();

    // Control: an ordinary request goes through the proxy, so the setting is live and the count means something.
    let out = env.run_with(&["postgres", "get", "--postgres-id", "pg_1", "-a", "work", "--json"], &via_proxy);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "an ordinary request should have used the proxy");

    // This command makes two requests to the mock: the allow-list read, which uses the proxy (one more tunnel),
    // and the address probe, which must not.
    let (gets, probes) = (count(&mock, "postgres/pg_1"), count(&mock, "me"));
    let args = ["postgres", "allowlist", "show", "--postgres-id", "pg_1", "--my-ip", "--ipv4", "-a", "work", "--json"];
    let out = env.run_with(&args, &via_proxy);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!((count(&mock, "postgres/pg_1"), count(&mock, "me")), (gets + 1, probes + 1), "both requests happened");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "the read went through the proxy; the address probe did not");
}

// ---------------------------------------------------------------------------------------------
// postgres query / connect
//
// The first group needs nothing but this machine: the mock API, and fake database ports that
// misbehave. The last test talks to a real PostgreSQL in Docker and is `#[ignore]`d; see its comment.

const PGQ_PASSWORD: &str = "s3cr3t-Passw0rd-xyz";

fn pgq_db(host: &str, port: u16, password: &str, allow: &[&str]) -> Value {
    let list: Vec<Value> = allow.iter().enumerate().map(|(i, s)| entry(&format!("pgfw_{i}"), s, "")).collect();
    json!({
        "id": "pg_1", "name": "main", "status": "running", "ipAllowList": list,
        "connection": {"host": host, "port": port, "database": "app", "user": "app", "password": password,
                       "uri": format!("postgres://app:{password}@{host}:{port}/app")},
    })
}

/// A mock API with one database, an account `work`, and the environment to run the CLI against it.
fn pgq_env(db: Value) -> (Mock, Env) {
    let mock = Mock::start(vec![me(), ("GET", "postgres/pg_1", 200, db)]);
    let env = Env::new(&mock).with_account();
    (mock, env)
}

struct Run {
    code: i32,
    out: Value,
    err: Value,
    /// Everything the command printed, both streams, for "this must never appear" checks.
    raw: String,
}

/// `sliplane postgres <args> --postgres-id pg_1 -a work --json`, with optional stdin and extra environment.
fn pgq(env: &Env, args: &[&str], stdin: Option<&str>, envs: &[(&str, &std::ffi::OsStr)]) -> Run {
    use std::process::Stdio;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sliplane"));
    cmd.arg("postgres")
        .args(args)
        .args(["--postgres-id", "pg_1", "-a", "work", "--json"])
        .env("SLIPLANE_CONFIG_DIR", &env.dir)
        .env("SLIPLANE_SECRET_STORE", "plaintext")
        .env("SLIPLANE_API_URL", &env.api)
        .env_remove("SLIPLANE_PG_CA_FILE")
        .envs(envs.iter().copied())
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(text) = stdin {
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let parse = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        let s =
            s.lines().filter(|l| !l.starts_with("warning:") && !l.starts_with("note:")).collect::<Vec<_>>().join("\n");
        serde_json::from_str(&s).unwrap_or(Value::Null)
    };
    Run {
        code: out.status.code().unwrap(),
        out: parse(&out.stdout),
        err: parse(&out.stderr),
        raw: format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    }
}

/// A TCP port that runs `behave` on each connection, and keeps what the client sent.
fn pgq_fake(behave: fn(&mut std::net::TcpStream, &Mutex<Vec<u8>>)) -> (u16, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            behave(&mut stream, &seen2);
        }
    });
    (port, seen)
}

/// Answers the SSL request with `N`, as a server without TLS does, then listens for anything else.
fn pgq_no_tls(stream: &mut std::net::TcpStream, seen: &Mutex<Vec<u8>>) {
    let mut request = [0u8; 8];
    stream.read_exact(&mut request).unwrap();
    seen.lock().unwrap().extend_from_slice(&request);
    stream.write_all(b"N").unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest);
    seen.lock().unwrap().extend_from_slice(&rest);
}

/// Hangs up at once, which is what a firewall's reset looks like from the client.
fn pgq_hang_up(_: &mut std::net::TcpStream, _: &Mutex<Vec<u8>>) {}

#[test]
fn postgres_query_checks_its_input_before_it_connects() {
    let (mock, env) = pgq_env(pgq_db("127.0.0.1", 1, PGQ_PASSWORD, &["0.0.0.0/0"]));
    let before = mock.requests().len();
    let invalid = |args: &[&str], stdin: Option<&str>, want: &str| {
        let r = pgq(&env, &[&["query"][..], args].concat(), stdin, &[]);
        assert_eq!((r.code, r.err["code"].as_str()), (6, Some("invalid_input")), "{args:?}: {}", r.raw);
        assert!(r.err["error"].as_str().unwrap().contains(want), "{args:?}: {}", r.raw);
    };
    invalid(&[], None, "Pass the SQL");
    invalid(&["--sql", "   "], None, "empty");
    invalid(&["--sql", "select 1", "--file", "x.sql"], None, "cannot be used with");
    invalid(&["--file", "/no/such/dir/x.sql"], None, "Could not read");
    invalid(&["--sql", "-"], Some("  \n"), "empty");
    invalid(&["--sql", "select 1", "--timeout", "soon"], None, "--timeout takes a duration");
    invalid(&["--sql", "select 1", "--timeout", "2d"], None, "at most 1d");
    invalid(&["--sql", "select 1", "--max-rows", "0"], None, "max-rows");
    // A NUL cannot travel in an argument, but it can arrive on stdin; that one is caught after the details are fetched.
    invalid(&["--sql", "-"], Some("select 1\u{0}"), "NUL");
    assert_eq!(mock.requests().len(), before + 1, "only the NUL case got as far as fetching the connection details");
}

#[test]
fn postgres_query_wants_a_password_a_running_database_and_a_name_that_exists() {
    // No secrets:read: the API blanks the password.
    let (_mock, env) = pgq_env(pgq_db("127.0.0.1", 1, "", &["0.0.0.0/0"]));
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!((r.code, r.err["code"].as_str()), (3, Some("auth_required")), "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("Secrets: Read"), "{}", r.raw);

    // Paused: no connection details at all.
    let paused = json!({"id": "pg_1", "name": "main", "status": "paused", "ipAllowList": []});
    let (_mock, env) = pgq_env(paused);
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("status: paused"));
    assert_eq!(
        r.err["remediation"],
        "sliplane postgres unpause --postgres-id pg_1 -a work, then wait for status running"
    );

    // A name is resolved like everywhere else (the database does not exist here, so the listing says so).
    let mock = Mock::start(vec![me(), ("GET", "postgres", 200, json!([{"id": "pg_9", "name": "other"}]))]);
    let env = Env::new(&mock).with_account();
    let out = Command::new(env!("CARGO_BIN_EXE_sliplane"))
        .args(["postgres", "query", "--postgres", "main", "--sql", "select 1", "-a", "work", "--json"])
        .env("SLIPLANE_CONFIG_DIR", &env.dir)
        .env("SLIPLANE_SECRET_STORE", "plaintext")
        .env("SLIPLANE_API_URL", &env.api)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn postgres_query_never_sends_credentials_to_a_server_without_tls() {
    let (port, seen) = pgq_fake(pgq_no_tls);
    let (_mock, env) = pgq_env(pgq_db("127.0.0.1", port, PGQ_PASSWORD, &["0.0.0.0/0"]));
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("does not offer TLS"), "{}", r.raw);
    assert!(!r.raw.contains(PGQ_PASSWORD));
    // All the server ever received was the 8-byte SSLRequest: length 8, code 80877103.
    assert_eq!(*seen.lock().unwrap(), [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f]);
}

#[test]
fn postgres_query_points_at_the_allow_list_when_the_connection_dies() {
    let (port, _) = pgq_fake(pgq_hang_up);
    let (_mock, env) = pgq_env(pgq_db("127.0.0.1", port, PGQ_PASSWORD, &["203.0.113.0/24"]));
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!((r.code, r.err["code"].as_str()), (2, Some("network")), "{}", r.raw);
    assert_eq!(
        r.err["remediation"],
        "If this machine's address is not on the IP allow list: sliplane postgres allowlist add --postgres-id pg_1 --my-ip --ipv4 -a work"
    );
    assert!(r.err["detail"].as_str().unwrap().contains("restricted to the addresses it lists"), "{}", r.raw);
    assert!(!r.raw.contains(PGQ_PASSWORD));

    // An allow list that lets everyone in is not the reason.
    let (_mock, env) = pgq_env(pgq_db("127.0.0.1", port, PGQ_PASSWORD, &["0.0.0.0/0"]));
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!(r.code, 2, "{}", r.raw);
    assert!(!r.err["remediation"].as_str().unwrap().contains("allowlist add"), "{}", r.raw);

    // Nothing listening is a different problem with a different answer.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (_mock, env) = pgq_env(pgq_db("127.0.0.1", closed, PGQ_PASSWORD, &["203.0.113.0/24"]));
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!(r.code, 2, "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("paused or restarting"), "{}", r.raw);
}

#[test]
fn postgres_connect_says_when_psql_is_missing_and_keeps_the_password_out_of_sight() {
    let (_mock, env) = pgq_env(pgq_db("127.0.0.1", 1, PGQ_PASSWORD, &["0.0.0.0/0"]));
    let empty = env.dir.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let r = pgq(&env, &["connect"], None, &[("PATH", empty.as_os_str())]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("psql was not found"), "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("postgres query"), "{}", r.raw);
    assert!(!r.raw.contains(PGQ_PASSWORD));
}

// --- against a real PostgreSQL ---------------------------------------------------------------

/// A throwaway PostgreSQL in Docker, with TLS from a CA made on the spot (so the CLI's certificate
/// checks are really exercised) and SCRAM passwords. Removed again on drop.
struct PgContainer {
    name: String,
    port: u16,
    ca: PathBuf,
}

fn docker(args: &[&str]) -> Output {
    Command::new("docker")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("this test needs Docker, and `docker` could not be run: {e}"))
}

impl PgContainer {
    fn start(dir: &std::path::Path) -> PgContainer {
        // `SLIPLANE_TEST_PG_IMAGE` picks another PostgreSQL; Sliplane's own default is 18.
        let image = std::env::var("SLIPLANE_TEST_PG_IMAGE").unwrap_or_else(|_| "postgres:18".into());
        let name = format!("sliplane-pgtest-{}", std::process::id());
        // A CA, a server certificate for `localhost` signed by it, and the server started with both.
        let script = "set -e; d=/var/lib/sliplane-tls; mkdir -p $d; cd $d; \
            openssl req -x509 -newkey rsa:2048 -nodes -days 3 -keyout ca.key -out ca.pem -subj /CN=sliplane-test-ca \
              -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign; \
            openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj /CN=localhost; \
            printf 'subjectAltName=DNS:localhost\\nbasicConstraints=CA:FALSE\\nextendedKeyUsage=serverAuth\\n' > ext.cnf; \
            openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 2 -out server.crt -extfile ext.cnf; \
            chown postgres:postgres server.key server.crt; chmod 600 server.key; \
            exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=$d/server.crt -c ssl_key_file=$d/server.key \
              -c password_encryption=scram-sha-256";
        let pg_password = format!("POSTGRES_PASSWORD={PGQ_PASSWORD}");
        let started = docker(&[
            "run",
            "-d",
            "--name",
            &name,
            "-e",
            "POSTGRES_USER=app",
            "-e",
            &pg_password,
            "-e",
            "POSTGRES_DB=app",
            "-p",
            "127.0.0.1::5432",
            "--entrypoint",
            "sh",
            &image,
            "-c",
            script,
        ]);
        assert!(started.status.success(), "docker run failed: {}", String::from_utf8_lossy(&started.stderr));
        // From here on, Drop removes the container, whatever happens.
        let mut pg = PgContainer { name, port: 0, ca: dir.join("ca.pem") };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            // `-h localhost`: the initialisation server only listens on a socket, so this is the real one.
            if docker(&["exec", &pg.name, "pg_isready", "-h", "localhost", "-U", "app", "-d", "app"]).status.success() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "PostgreSQL did not come up");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let mapped = String::from_utf8(docker(&["port", &pg.name, "5432/tcp"]).stdout).unwrap();
        pg.port = mapped.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.trim().parse().ok()).unwrap();
        let ca = docker(&["exec", &pg.name, "cat", "/var/lib/sliplane-tls/ca.pem"]);
        assert!(ca.status.success());
        std::fs::write(&pg.ca, ca.stdout).unwrap();
        pg
    }
}

impl Drop for PgContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}

/// Runs against a real PostgreSQL in Docker (`postgres:18`, or `SLIPLANE_TEST_PG_IMAGE`). Not part of
/// `cargo test`, because CI has no Docker. Run it with:
///
///     cargo test --test cli postgres_query_against_a_real_postgres -- --ignored --nocapture
///
/// One test, not many, because starting the server takes seconds; the steps run in order and share
/// the database (the table made with `--write` is read back later).
#[test]
#[ignore = "needs Docker: cargo test --test cli postgres_query_against_a_real_postgres -- --ignored"]
fn postgres_query_against_a_real_postgres() {
    let dir = std::env::temp_dir().join(format!("sliplane-pgq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pg = PgContainer::start(&dir);
    let ca = pg.ca.clone().into_os_string();
    let trust: Vec<(&str, &std::ffi::OsStr)> = vec![("SLIPLANE_PG_CA_FILE", ca.as_os_str())];

    let (_mock, env) = pgq_env(pgq_db("localhost", pg.port, PGQ_PASSWORD, &["0.0.0.0/0"]));
    let every_output = std::cell::RefCell::new(Vec::new());
    let q = |args: &[&str]| {
        let r = pgq(&env, &[&["query"][..], args].concat(), None, &trust);
        every_output.borrow_mut().push(r.raw.clone());
        r
    };

    // 1. Values of many types, as data.
    let r = q(&[
        "--sql",
        "select 1::int2 as i2, 2::int4 as i4, 9223372036854775807::int8 as i8, 1.5::float8 as f8, \
        'NaN'::float8 as nan, 12345678901234567890.123456789::numeric as n, true as b, null::text as nul, \
        '{\"a\":[1,{\"b\":null}]}'::jsonb as j, 'h\u{e9}llo'::text as t, '2024-01-02'::date as d, array[1,2]::int4[] as arr",
    ]);
    assert_eq!(r.code, 0, "{}", r.raw);
    assert_eq!(r.out["readOnly"], true);
    let st = &r.out["statements"][0];
    assert_eq!(
        (st["commandTag"].as_str(), st["rowCount"].as_u64(), st["truncated"].as_bool()),
        (Some("SELECT 1"), Some(1), Some(false))
    );
    let row = &st["rows"][0];
    assert_eq!((row["i2"].as_i64(), row["i4"].as_i64(), row["i8"].as_i64()), (Some(1), Some(2), Some(i64::MAX)));
    assert_eq!(row["f8"], 1.5);
    assert_eq!(row["nan"], "NaN");
    assert_eq!(row["n"], "12345678901234567890.123456789");
    assert_eq!(row["b"], true);
    assert!(row["nul"].is_null());
    assert_eq!(row["j"], json!({"a": [1, {"b": null}]}));
    assert_eq!(
        (row["t"].as_str(), row["d"].as_str(), row["arr"].as_str()),
        (Some("h\u{e9}llo"), Some("2024-01-02"), Some("{1,2}"))
    );
    assert_eq!(st["columns"][2], json!({"name": "i8", "type": "int8", "oid": 20}));
    assert_eq!(st["columns"][5]["type"], "numeric");

    // 2. An empty result is still a result; several statements are several results.
    let r = q(&["--sql", "select 1 as x where false; select 'a' as y, 'b' as z"]);
    assert_eq!(r.code, 0, "{}", r.raw);
    assert_eq!(r.out["statements"][0]["rows"], json!([]));
    assert_eq!(r.out["statements"][0]["columns"][0]["name"], "x");
    assert_eq!(r.out["statements"][1]["rows"], json!([{"y": "a", "z": "b"}]));

    // 3. Row cap.
    let r = q(&["--sql", "select g from generate_series(1, 10) g", "--max-rows", "3"]);
    let st = &r.out["statements"][0];
    assert_eq!(
        (st["rows"].as_array().unwrap().len(), st["rowCount"].as_u64(), st["truncated"].as_bool()),
        (3, Some(10), Some(true)),
        "{}",
        r.raw
    );

    //    A big result under the default cap: everything is counted, a thousand rows are kept.
    let r = q(&["--sql", "select g, repeat('x', 50) as pad from generate_series(1, 20000) g"]);
    let st = &r.out["statements"][0];
    assert_eq!(
        (st["rows"].as_array().unwrap().len(), st["rowCount"].as_u64(), st["truncated"].as_bool()),
        (1000, Some(20_000), Some(true))
    );
    assert_eq!(st["rows"][999]["g"], 1000);

    // 3b. The server hanging up on us mid-query is an error with its SQLSTATE, not a hang or a panic.
    let r = q(&["--sql", "select pg_terminate_backend(pg_backend_pid())"]);
    assert_eq!(r.code, 1, "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("57P01"), "{}", r.raw);

    // 4. Repeated column names do not overwrite each other.
    let r = q(&["--sql", "select 1 as a, 2 as a"]);
    let st = &r.out["statements"][0];
    assert_eq!(st["rows"][0], json!({"a": 1, "a_2": 2}));
    assert_eq!(st["columns"][1]["key"], "a_2");
    assert!(st["warnings"][0].as_str().unwrap().contains("a_2"));

    // 5. Read-only by default: a refusal that says how to get past it.
    let r = q(&["--sql", "create table qt(id int, note text)"]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("25006"), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("read-only transaction"), "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("--write"), "{}", r.raw);

    // 6. --write allows it, several statements in one call.
    let r = q(&[
        "--write",
        "--sql",
        "create table qt(id int, note text); insert into qt values (1, 'a'), (2, null); select * from qt order by id",
    ]);
    assert_eq!(r.code, 0, "{}", r.raw);
    assert_eq!(r.out["readOnly"], false);
    let tags: Vec<_> =
        r.out["statements"].as_array().unwrap().iter().map(|s| s["commandTag"].as_str().unwrap().to_string()).collect();
    assert_eq!(tags, ["CREATE TABLE", "INSERT 0 2", "SELECT 2"]);
    assert_eq!(r.out["statements"][1]["rowCount"], 2);
    assert_eq!(r.out["statements"][2]["rows"], json!([{"id": 1, "note": "a"}, {"id": 2, "note": null}]));

    // 7. ...and read-only sessions still read it, but cannot insert.
    let r = q(&["--sql", "select count(*) as n from qt"]);
    assert_eq!(r.out["statements"][0]["rows"][0]["n"], 2, "{}", r.raw);
    let r = q(&["--sql", "insert into qt values (3, 'c')"]);
    assert_eq!(r.code, 1, "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("--write"));

    // 8. A failing statement: SQLSTATE, position, which statement; and what came before it is rolled back.
    let r = q(&["--write", "--sql", "insert into qt values (9, 'z'); select * from does_not_exist"]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("42P01"), "{}", r.raw);
    let detail = r.err["detail"].as_str().unwrap();
    for want in ["severity: ERROR", "sqlstate: 42P01", "position: ", "statement: 2", "rolled back"] {
        assert!(detail.contains(want), "{want} missing from {detail}");
    }
    let r = q(&["--sql", "select count(*) as n from qt where id = 9"]);
    assert_eq!(
        r.out["statements"][0]["rows"][0]["n"], 0,
        "the insert before the failure must not have stuck: {}",
        r.raw
    );

    // 9. Notices come back with their statement.
    let r = q(&["--write", "--sql", "create table if not exists qt(id int)"]);
    assert!(r.out["statements"][0]["notices"][0].as_str().unwrap().contains("already exists, skipping"), "{}", r.raw);

    // 10. SQL from stdin and from a file.
    let r = pgq(&env, &["query", "--sql", "-"], Some("select 42 as answer"), &trust);
    assert_eq!(r.out["statements"][0]["rows"][0]["answer"], 42, "{}", r.raw);
    let file = dir.join("q.sql");
    std::fs::write(&file, "-- a comment first\nselect 43 as answer").unwrap();
    let r = pgq(&env, &["query", "--file", file.to_str().unwrap()], None, &trust);
    assert_eq!(r.out["statements"][0]["rows"][0]["answer"], 43, "{}", r.raw);

    // 11. The statement timeout is enforced, and the error says which flag to raise.
    let started = std::time::Instant::now();
    let r = q(&["--sql", "select pg_sleep(20)", "--timeout", "1s"]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("57014"), "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("--timeout"), "{}", r.raw);
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "took {:?}", started.elapsed());

    // 12. A syntax error is a SQL error like any other.
    let r = q(&["--sql", "selec 1"]);
    assert!(r.err["error"].as_str().unwrap().contains("42601"), "{}", r.raw);
    assert!(r.err["detail"].as_str().unwrap().contains("position: 1"), "{}", r.raw);

    // 13. A wrong password is refused by the server; neither password is printed.
    let (_m, wrong) = pgq_env(pgq_db("localhost", pg.port, "not-the-password-0000", &["0.0.0.0/0"]));
    let r = pgq(&wrong, &["query", "--sql", "select 1"], None, &trust);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("28P01"), "{}", r.raw);
    assert!(!r.raw.contains("not-the-password-0000"), "{}", r.raw);
    every_output.borrow_mut().push(r.raw);

    // 14. Verification is on: a host name the certificate does not carry is refused,
    let (_m, by_ip) = pgq_env(pgq_db("127.0.0.1", pg.port, PGQ_PASSWORD, &["0.0.0.0/0"]));
    let r = pgq(&by_ip, &["query", "--sql", "select 1"], None, &trust);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("not valid for the host name '127.0.0.1'"), "{}", r.raw);
    every_output.borrow_mut().push(r.raw);
    //     and so is a certificate from an authority nobody told it to trust.
    let r = pgq(&env, &["query", "--sql", "select 1"], None, &[]);
    assert_eq!((r.code, r.err["code"].as_str()), (1, Some("error")), "{}", r.raw);
    assert!(r.err["error"].as_str().unwrap().contains("authority this machine does not trust"), "{}", r.raw);
    assert!(r.err["remediation"].as_str().unwrap().contains("SLIPLANE_PG_CA_FILE"), "{}", r.raw);
    every_output.borrow_mut().push(r.raw);

    // The password, in all of it.
    for out in every_output.borrow().iter() {
        assert!(!out.contains(PGQ_PASSWORD), "the password leaked into: {out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
