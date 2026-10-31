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
    assert_eq!(body["deployment"], json!({"url": "docker.io/library/nginx:latest"}));
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
// --env-file and --secret-env-file

/// Writes a file into the test's own directory and returns its path.
fn file_in(env: &Env, name: &str, text: &str) -> String {
    let path = env.dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

const CREATE: [&str; 11] =
    ["services", "create", "--project-id", "p1", "--name", "web", "--server-id", "srv1", "--image", "nginx", "-a"];

fn create_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = CREATE.to_vec();
    args.push("work");
    args.extend(extra);
    args
}

#[test]
fn create_reads_plain_and_secret_variables_from_files_and_the_flag_wins() {
    let mock = Mock::start(vec![me(), ("POST", "projects/p1/services", 201, json!({"id": "s9"}))]);
    let env = Env::new(&mock).with_account();
    let plain = file_in(&env, "plain.env", "# app\nPORT=8080\nexport MODE=\"a b\"\nA=from-file\n");
    let secret = file_in(&env, "secret.env", "DB_PASSWORD='p#ss $word'\n");
    let out = env.run(&create_args(&[
        "--env-file",
        &plain,
        "--secret-env-file",
        &secret,
        "--env",
        "A=from-flag",
        "--secret-env",
        "S=2",
        "--json",
    ]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");

    let body = mock.last("POST").body.unwrap();
    assert_eq!(
        body["env"],
        json!([
            {"key": "PORT", "value": "8080", "secret": false},
            {"key": "MODE", "value": "a b", "secret": false},
            {"key": "DB_PASSWORD", "value": "p#ss $word", "secret": true},
            {"key": "A", "value": "from-flag", "secret": false},
            {"key": "S", "value": "2", "secret": true},
        ])
    );
    // The override is named, with where it came from, and no value is printed.
    assert!(
        stderr.contains(&format!("note: A at {plain}:4 is replaced by the A given on the command line")),
        "{stderr}"
    );
    for value in ["from-file", "from-flag", "p#ss"] {
        assert!(!stderr.contains(value), "a value reached stderr: {stderr}");
    }
}

#[test]
fn later_files_replace_earlier_ones_and_say_so() {
    let mock = Mock::start(vec![me(), ("POST", "projects/p1/services", 201, json!({"id": "s9"}))]);
    let env = Env::new(&mock).with_account();
    let first = file_in(&env, "first.env", "A=1\nB=1\n");
    let second = file_in(&env, "second.env", "B=2\nB=3\n");
    let out = env.run(&create_args(&["--env-file", &first, "--env-file", &second, "--json"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        mock.last("POST").body.unwrap()["env"],
        json!([{"key": "A", "value": "1", "secret": false}, {"key": "B", "value": "3", "secret": false}])
    );
    assert!(stderr.contains(&format!("note: B at {second}:1 replaces B at {first}:2")), "{stderr}");
    assert!(stderr.contains(&format!("note: B at {second}:2 replaces B at {second}:1")), "{stderr}");
}

#[test]
fn a_bad_env_file_names_the_line_and_never_the_value_and_creates_nothing() {
    let mock = Mock::start(vec![me(), ("POST", "projects/p1/services", 201, json!({"id": "s9"}))]);
    let env = Env::new(&mock).with_account();

    // A bare KEY means "empty value" for --env; in a file it is refused, not silently reinterpreted.
    let bare = file_in(&env, "bare.env", "A=1\nSUPERSECRETTOKEN\n");
    let (code, _, err) = env.json(&create_args(&["--env-file", &bare]));
    assert_eq!(code, 6);
    let message = err["error"].as_str().unwrap();
    assert!(message.contains("bare.env, line 2"), "{err}");
    assert!(!message.contains("SUPERSECRETTOKEN"), "the line was echoed: {err}");

    let open = file_in(&env, "open.env", "A=1\nB=\"hunter2 never closed\nC=3\n");
    let (code, _, err) = env.json(&create_args(&["--secret-env-file", &open]));
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("open.env, line 2"), "{err}");
    assert!(!err.to_string().contains("hunter2"), "{err}");

    let missing = env.dir.join("nope.env").to_string_lossy().into_owned();
    let (code, _, err) = env.json(&create_args(&["--env-file", &missing]));
    assert_eq!(code, 6, "{err}");

    let empty = file_in(&env, "empty.env", "# nothing here\n");
    let (code, _, err) = env.json(&create_args(&["--env-file", &empty]));
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("defines no variables"), "{err}");

    // Every one of those failed before the account was asked anything but `me`.
    assert_eq!(paths(&mock, "POST"), Vec::<String>::new());
    assert_eq!(paths(&mock, "GET"), ["me"], "files are read before any lookup");
}

#[test]
fn update_applies_the_env_protections_to_file_variables_too() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let file = file_in(&env, "new.env", "A=9\n");
    let base = ["services", "update", "--project-id", "p1", "--service-id", "s1", "--env-file", &file, "-a", "work"];

    // Without a flag saying what to do with B and S, the update is refused and names them.
    let (code, _, err) = env.json(&base);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("2 environment variable(s) not listed: B, S"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "PATCH"));

    // --merge-env keeps them, secrets sent back empty so the API keeps the stored value.
    let mut args = base.to_vec();
    args.push("--merge-env");
    let (code, _, err) = env.json(&args);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PATCH").body.unwrap()["env"],
        json!([{"key": "B", "value": "2", "secret": false},
               {"key": "S", "value": "", "secret": true},
               {"key": "A", "value": "9", "secret": false}])
    );

    // --replace-env sends exactly the file.
    let mut args = base.to_vec();
    args.push("--replace-env");
    let (code, _, err) = env.json(&args);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap()["env"], json!([{"key": "A", "value": "9", "secret": false}]));
}

#[test]
fn update_merge_with_a_key_that_cannot_read_secrets_still_refuses_for_file_variables() {
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:write"])),
        ("GET", SVC, 200, repo_service()),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let file = file_in(&env, "new.env", "A=9\n");
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--secret-env-file",
        &file,
        "--merge-env",
        "-a",
        "work",
    ]);
    assert_eq!(code, 3, "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "PATCH"));
}

#[test]
fn update_with_an_env_file_and_no_variables_in_it_is_not_a_silent_no_op() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let file = file_in(&env, "empty.env", "\n");
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--env-file",
        &file,
        "--merge-env",
        "-a",
        "work",
    ]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("defines no variables"), "{err}");
}

#[test]
fn presets_deploy_takes_env_files_as_overrides() {
    let mock = Mock::start(vec![me(), catalog(), ("POST", "projects/p1/services", 201, json!({"id": "svc1"}))]);
    let env = Env::new(&mock).with_account();
    let secrets = file_in(&env, "keys.env", "OPENAI_API_KEY=sk-from-file\n");
    let (code, _, err) = env.json(&[
        "presets",
        "deploy",
        "n8n",
        "--project-id",
        "p1",
        "--server-id",
        "srv1",
        "--secret-env-file",
        &secrets,
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["env"], json!([{"key": "OPENAI_API_KEY", "value": "sk-from-file", "secret": true}]));
}

// ---------------------------------------------------------------------------------------------
// credentials create: the token on stdin

impl Env {
    /// Runs the binary with `stdin` piped to it (or closed, when None), capturing both streams.
    fn run_with_stdin(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sliplane"))
            .args(args)
            .env("SLIPLANE_CONFIG_DIR", &self.dir)
            .env("SLIPLANE_SECRET_STORE", "plaintext")
            .env("SLIPLANE_API_URL", &self.api)
            .stdin(if stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(bytes) = stdin {
            // The child may stop reading early (an oversized token): a broken pipe is fine.
            let _ = child.stdin.take().unwrap().write_all(bytes);
        }
        child.wait_with_output().unwrap()
    }
}

const CRED_BASE: [&str; 9] =
    ["credentials", "create", "--name", "gh", "--type", "ghcr", "--username", "octo", "--json"];

fn credential_routes() -> Vec<Route> {
    vec![
        me(),
        (
            "POST",
            "registry-credentials",
            201,
            json!({"id": "credential_1", "name": "gh", "type": "ghcr", "username": "octo", "createdAt": "2026-10-31T12:00:00Z"}),
        ),
    ]
}

#[test]
fn credentials_token_comes_from_stdin_and_never_touches_argv() {
    let mock = Mock::start(credential_routes());
    let env = Env::new(&mock).with_account();
    let mut args = CRED_BASE.to_vec();
    args.extend(["--token-stdin", "-a", "work"]);

    // The line break `printf`/`echo` adds is not part of the token; a CRLF from Windows is not either.
    let out = env.run_with_stdin(&args, Some(b"ghp_secret value \r\n"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        mock.last("POST").body.unwrap(),
        json!({"name": "gh", "type": "ghcr", "username": "octo", "token": "ghp_secret value "})
    );
    assert!(!stderr.contains("registry token in the process list"), "stdin is the right way, so no nagging: {stderr}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("ghp_secret"), "the token was printed");
}

#[test]
fn credentials_token_on_argv_still_works_but_warns_without_repeating_it() {
    let mock = Mock::start(credential_routes());
    let env = Env::new(&mock).with_account();
    let mut args = CRED_BASE.to_vec();
    args.extend(["--token", "ghp_argv_secret", "-a", "work"]);
    let out = env.run_with_stdin(&args, None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(mock.last("POST").body.unwrap()["token"], "ghp_argv_secret");
    assert!(stderr.contains("warning: --token puts the registry token in the process list"), "{stderr}");
    assert!(stderr.contains("--token-stdin"), "{stderr}");
    assert!(!stderr.contains("ghp_argv_secret"), "the warning must not echo the token: {stderr}");
}

#[test]
fn credentials_token_is_required_from_exactly_one_place() {
    let mock = Mock::start(credential_routes());
    let env = Env::new(&mock).with_account();

    // Nothing given: say how, and do not look at the account or the network.
    let mut args = CRED_BASE.to_vec();
    args.extend(["-a", "work"]);
    let out = env.run_with_stdin(&args, None);
    assert_eq!(out.status.code(), Some(6));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(err["remediation"].as_str().unwrap().contains("--token-stdin"), "{err}");

    // --token-stdin with nothing piped (stdin closed) must fail, not hang.
    let mut args = CRED_BASE.to_vec();
    args.extend(["--token-stdin", "-a", "work"]);
    let out = env.run_with_stdin(&args, None);
    assert_eq!(out.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&out.stderr).contains("stdin was empty"));
    let out = env.run_with_stdin(&args, Some(b"\n"));
    assert_eq!(out.status.code(), Some(6), "a blank line is not a token");

    // Both: refused by the parser.
    let mut args = CRED_BASE.to_vec();
    args.extend(["--token", "x", "--token-stdin", "-a", "work"]);
    assert_eq!(env.run_with_stdin(&args, Some(b"y")).status.code(), Some(6));

    // Oversized input is not a token.
    let mut args = CRED_BASE.to_vec();
    args.extend(["--token-stdin", "-a", "work"]);
    let out = env.run_with_stdin(&args, Some(&vec![b'a'; 70 * 1024]));
    assert_eq!(out.status.code(), Some(6));

    assert_eq!(paths(&mock, "POST"), Vec::<String>::new(), "none of those reached the API");
}

#[test]
fn credentials_errors_do_not_echo_the_token_back() {
    let mock = Mock::start(vec![
        me(),
        (
            "POST",
            "registry-credentials",
            400,
            json!({"code": "invalid_request", "message": "token ghp_leaky is not valid"}),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let mut args = CRED_BASE.to_vec();
    args.extend(["--token-stdin", "-a", "work"]);
    let out = env.run_with_stdin(&args, Some(b"ghp_leaky"));
    assert_eq!(out.status.code(), Some(6));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("ghp_leaky"), "{stderr}");
    assert!(stderr.contains("[redacted]"), "{stderr}");
}

// ---------------------------------------------------------------------------------------------
// --image shorthand

#[test]
fn create_expands_docker_hub_shorthand_and_says_so() {
    let mock = Mock::start(vec![me(), ("POST", "projects/p1/services", 201, json!({"id": "s9"}))]);
    let env = Env::new(&mock).with_account();
    let sent = |image: &str| {
        let out = env.run(&create_args_with_image(image));
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(0), "{stderr}");
        (mock.last("POST").body.unwrap()["deployment"]["url"].as_str().unwrap().to_string(), stderr)
    };

    let (url, stderr) = sent("nginx");
    assert_eq!(url, "docker.io/library/nginx:latest");
    assert!(stderr.contains("note: --image 'nginx' was sent as 'docker.io/library/nginx:latest'"), "{stderr}");

    assert_eq!(sent("nginx:1.27").0, "docker.io/library/nginx:1.27");
    assert_eq!(sent("user/app").0, "docker.io/user/app:latest");

    // A registry host means the reference is complete: sent as written, and no note.
    for image in ["ghcr.io/o/r:tag", "localhost:5000/app", "docker.io/library/nginx:1.27"] {
        let (url, stderr) = sent(image);
        assert_eq!(url, image);
        assert!(!stderr.contains("note: --image"), "{image}: {stderr}");
    }
    let digest = "ghcr.io/o/app@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(sent(digest).0, digest);
    let hub_digest = "nginx@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(sent(hub_digest).0, format!("docker.io/library/{hub_digest}"));
}

fn create_args_with_image(image: &str) -> Vec<&str> {
    vec![
        "services",
        "create",
        "--project-id",
        "p1",
        "--name",
        "web",
        "--server-id",
        "srv1",
        "--image",
        image,
        "-a",
        "work",
        "--json",
    ]
}

#[test]
fn update_expands_the_image_and_keeps_credentials_in_the_same_registry() {
    let current =
        json!({"id": "s1", "deployment": {"url": "docker.io/acme/app:1", "registryAuthenticationId": "cred_1"}});
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, current.clone()), ("PATCH", SVC, 200, current)]);
    let env = Env::new(&mock).with_account();
    let update = |image: &str| {
        let out = env.run(&[
            "services",
            "update",
            "--project-id",
            "p1",
            "--service-id",
            "s1",
            "--image",
            image,
            "-a",
            "work",
            "--json",
        ]);
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        (mock.last("PATCH").body.unwrap()["deployment"].clone(), String::from_utf8_lossy(&out.stderr).into_owned())
    };

    // `acme/app:2` is Docker Hub like the stored `docker.io/acme/app:1`, so the credentials stay.
    let (dep, stderr) = update("acme/app:2");
    assert_eq!(dep, json!({"url": "docker.io/acme/app:2", "registryAuthenticationId": "cred_1"}));
    assert!(stderr.contains("note: --image 'acme/app:2' was sent as 'docker.io/acme/app:2'"), "{stderr}");

    // Another registry drops them, exactly as before.
    let (dep, stderr) = update("ghcr.io/acme/app:2");
    assert_eq!(dep, json!({"url": "ghcr.io/acme/app:2"}));
    assert!(!stderr.contains("note: --image"), "{stderr}");
}

#[test]
fn a_rejected_image_command_says_nothing_about_the_image() {
    // --image with --repo is refused before the value is looked at, so no note is printed.
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let out = env.run(&[
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
        "--repo",
        "https://github.com/o/r",
        "-a",
        "work",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(6));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("note: --image"));
}

// ---------------------------------------------------------------------------------------------
// agent init

impl Env {
    /// A fresh project directory to run in; `agent init` writes relative to the working directory.
    fn project(&self, name: &str) -> PathBuf {
        let dir = self.dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sliplane"))
            .args(args)
            .current_dir(dir)
            .env("SLIPLANE_CONFIG_DIR", &self.dir)
            .env("SLIPLANE_SECRET_STORE", "plaintext")
            .env("SLIPLANE_API_URL", &self.api)
            .output()
            .unwrap()
    }
}

fn status_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn agent_init_creates_the_file_and_a_rerun_changes_nothing() {
    // No account, no routes: it must not need either, nor make a request.
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("fresh");

    let out = env.run_in(&dir, &["agent", "init", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(status_of(&out), json!({"status": "created", "path": "AGENTS.md"}));
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(text.starts_with("<!-- sliplane:begin -->\n") && text.trim_end().ends_with("<!-- sliplane:end -->"));
    for needed in ["sliplane agent-readme", "--account", "API key", "--json"] {
        assert!(text.contains(needed), "{needed}: {text}");
    }

    let out = env.run_in(&dir, &["agent", "init", "--json"]);
    assert_eq!(status_of(&out)["status"], "unchanged");
    assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), text);
    assert!(mock.requests().is_empty(), "agent init must not touch the network");
}

#[test]
fn agent_init_replaces_its_section_in_place_and_keeps_everything_else() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("existing");
    let mine_before = "# My project\n\nRules I wrote.\n\n";
    let mine_after = "\n## Other tooling\n\n- keep me";
    std::fs::write(
        dir.join("AGENTS.md"),
        format!("{mine_before}<!-- sliplane:begin -->\nan old, stale version\n<!-- sliplane:end -->{mine_after}"),
    )
    .unwrap();

    let out = env.run_in(&dir, &["agent", "init", "--json"]);
    assert_eq!(status_of(&out)["status"], "updated");
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(text.starts_with(mine_before) && text.ends_with(mine_after), "{text}");
    assert!(text.contains("sliplane agent-readme") && !text.contains("stale"), "{text}");
    assert_eq!(text.matches("sliplane:begin").count(), 1);

    // Text with no markers gets the section appended, and the original text stays as it was.
    std::fs::write(dir.join("CLAUDE.md"), "# Notes\nno trailing blank line").unwrap();
    let out = env.run_in(&dir, &["agent", "init", "--file", "CLAUDE.md", "--json"]);
    assert_eq!(status_of(&out), json!({"status": "updated", "path": "CLAUDE.md"}));
    let text = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert!(text.starts_with("# Notes\nno trailing blank line\n\n<!-- sliplane:begin -->"), "{text}");
}

#[test]
fn agent_init_print_shows_the_section_and_writes_nothing() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("printing");

    let out = env.run_in(&dir, &["agent", "init", "--print"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("<!-- sliplane:begin -->") && text.contains("sliplane agent-readme"), "{text}");
    assert!(!dir.join("AGENTS.md").exists());

    let out = env.run_in(&dir, &["agent", "init", "--print", "--json"]);
    assert!(status_of(&out)["content"].as_str().unwrap().ends_with("<!-- sliplane:end -->"));
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "--print wrote something");
}

#[test]
fn agent_init_will_not_write_outside_the_working_directory() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("inside");
    let outside = env.dir.join("escaped.md");
    let outside_path = outside.to_string_lossy().into_owned();

    for file in ["../escaped.md", "sub/../../escaped.md", outside_path.as_str()] {
        let out = env.run_in(&dir, &["agent", "init", "--file", file, "--json"]);
        assert_eq!(out.status.code(), Some(6), "{file}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(!outside.exists(), "{file} escaped");
    }

    // Only Markdown, only regular files, and the directory has to exist.
    for (file, why) in [("script.sh", "Markdown"), ("nowhere/AGENTS.md", "does not exist"), ("", "empty")] {
        let out = env.run_in(&dir, &["agent", "init", "--file", file, "--json"]);
        assert_eq!(out.status.code(), Some(6), "{file}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(why), "{file}: {}", String::from_utf8_lossy(&out.stderr));
    }
    std::fs::create_dir(dir.join("dir.md")).unwrap();
    assert_eq!(env.run_in(&dir, &["agent", "init", "--file", "dir.md", "--json"]).status.code(), Some(6));

    // A file inside a subdirectory of the working directory is fine.
    std::fs::create_dir(dir.join("docs")).unwrap();
    let out = env.run_in(&dir, &["agent", "init", "--file", "docs/AGENTS.md", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(dir.join("docs").join("AGENTS.md").exists());
}

#[cfg(unix)]
#[test]
fn agent_init_will_not_follow_a_symbolic_link_out_of_the_tree() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("linked");
    let elsewhere = env.project("elsewhere");
    std::fs::write(elsewhere.join("target.md"), "not yours").unwrap();
    std::os::unix::fs::symlink(elsewhere.join("target.md"), dir.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("linkdir")).unwrap();

    assert_eq!(env.run_in(&dir, &["agent", "init", "--json"]).status.code(), Some(6));
    assert_eq!(env.run_in(&dir, &["agent", "init", "--file", "linkdir/new.md", "--json"]).status.code(), Some(6));
    assert_eq!(std::fs::read_to_string(elsewhere.join("target.md")).unwrap(), "not yours");
    assert!(!elsewhere.join("new.md").exists());
}

#[test]
fn agent_init_refuses_broken_markers_and_leaves_the_file_alone() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("broken");
    let original = "# Mine\n<!-- sliplane:begin -->\nhalf a section\n";
    std::fs::write(dir.join("AGENTS.md"), original).unwrap();

    let out = env.run_in(&dir, &["agent", "init", "--json"]);
    assert_eq!(out.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing was written"));
    assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), original);

    // Not text at all: also left alone.
    std::fs::write(dir.join("AGENTS.md"), b"\xff\xfe binary").unwrap();
    assert_eq!(env.run_in(&dir, &["agent", "init", "--json"]).status.code(), Some(6));
    assert_eq!(std::fs::read(dir.join("AGENTS.md")).unwrap(), b"\xff\xfe binary");
}

#[test]
fn agent_init_keeps_crlf_files_crlf() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let dir = env.project("crlf");
    std::fs::write(dir.join("AGENTS.md"), "# Mine\r\n\r\ntext\r\n").unwrap();
    assert_eq!(status_of(&env.run_in(&dir, &["agent", "init", "--json"]))["status"], "updated");
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(text.starts_with("# Mine\r\n\r\ntext\r\n\r\n<!-- sliplane:begin -->\r\n"), "{text:?}");
    assert!(!text.replace("\r\n", "").contains('\n'), "mixed line endings: {text:?}");
    assert_eq!(status_of(&env.run_in(&dir, &["agent", "init", "--json"]))["status"], "unchanged");
}
