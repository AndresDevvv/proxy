use serde::Deserialize;
use serde_json::Value;
use worker::*;

#[derive(Deserialize)]
struct ProxyRequest {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    cookies: Option<Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    follow_redirects: Option<bool>,
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if req.method() == Method::Options {
        return cors(Response::empty()?);
    }

    let expected = env
        .secret("API_KEY")
        .map(|v| v.to_string())
        .unwrap_or_default();
    let provided = req
        .headers()
        .get("X-API-Key")
        .ok()
        .flatten()
        .or_else(|| {
            req.headers()
                .get("Authorization")
                .ok()
                .flatten()
                .map(|a| a.trim_start_matches("Bearer ").to_string())
        })
        .unwrap_or_default();
    if expected.is_empty() || !constant_eq(provided.as_bytes(), expected.as_bytes()) {
        return cors(json_err("unauthorized", 401)?);
    }

    let target;
    let method;
    let headers = Headers::new();
    let mut body: Option<String> = None;
    let mut redirect = RequestRedirect::Follow;

    let path = req.path();

    if path == "/" || path.is_empty() {
        if req.method() == Method::Post {
            let spec: ProxyRequest = match req.json().await {
                Ok(s) => s,
                Err(e) => return cors(json_err(&format!("invalid json: {e}"), 400)?),
            };

            target = spec.url;
            method = spec
                .method
                .map(|m| m.to_uppercase())
                .unwrap_or_else(|| "GET".into());

            if let Some(Value::Object(map)) = spec.headers {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        headers.set(&k, s)?;
                    }
                }
            }

            if let Some(Value::Object(map)) = spec.cookies {
                let cookie = map
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| format!("{k}={s}")))
                    .collect::<Vec<_>>()
                    .join("; ");
                if !cookie.is_empty() {
                    headers.set("Cookie", &cookie)?;
                }
            }

            body = spec.body;

            if spec.follow_redirects == Some(false) {
                redirect = RequestRedirect::Manual;
            }
        } else {
            return cors(usage()?);
        }
    } else {
        let url = req.url()?;
        let raw = url.as_str();
        let after = raw.splitn(2, "://").nth(1).unwrap_or("");
        let stripped = after.splitn(2, '/').nth(1).unwrap_or("");
        target = stripped.to_string();
        method = req.method().to_string();

        for (k, v) in req.headers().entries() {
            let lk = k.to_lowercase();
            if lk == "host" || lk == "cf-connecting-ip" || lk.starts_with("cf-") {
                continue;
            }
            headers.set(&k, &v).ok();
        }

        if req.method() != Method::Get && req.method() != Method::Head {
            body = Some(req.text().await.unwrap_or_default());
        }
    }

    if target.is_empty() {
        return cors(json_err("missing target url", 400)?);
    }

    let m = match Method::from(method.clone()) {
        m => m,
    };

    let mut init = RequestInit::new();
    init.with_method(m).with_headers(headers).with_redirect(redirect);

    if let Some(b) = body {
        if !b.is_empty() {
            init.with_body(Some(b.into()));
        }
    }

    let proxy_req = Request::new_with_init(&target, &init)?;
    let mut upstream = Fetch::Request(proxy_req).send().await?;

    let status = upstream.status_code();
    let bytes = upstream.bytes().await?;

    let out_headers = Headers::new();
    for (k, v) in upstream.headers().entries() {
        let lk = k.to_lowercase();
        if lk == "content-encoding" || lk == "content-length" || lk == "transfer-encoding" {
            continue;
        }
        out_headers.append(&k, &v).ok();
    }
    out_headers.set("Access-Control-Allow-Origin", "*")?;
    out_headers.set("Access-Control-Expose-Headers", "*")?;

    let resp = Response::from_bytes(bytes)?
        .with_status(status)
        .with_headers(out_headers);
    Ok(resp)
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn cors(resp: Response) -> Result<Response> {
    let mut r = resp;
    let h = r.headers_mut();
    h.set("Access-Control-Allow-Origin", "*")?;
    h.set("Access-Control-Allow-Methods", "*")?;
    h.set("Access-Control-Allow-Headers", "*")?;
    Ok(r)
}

fn json_err(msg: &str, status: u16) -> Result<Response> {
    Ok(Response::from_json(&serde_json::json!({ "error": msg }))?.with_status(status))
}

fn usage() -> Result<Response> {
    Response::from_json(&serde_json::json!({
        "usage": {
            "simple": "GET /<full-url>  e.g. /https://example.com",
            "advanced": "POST / with JSON { url, method, headers, cookies, body, follow_redirects }"
        }
    }))
}
