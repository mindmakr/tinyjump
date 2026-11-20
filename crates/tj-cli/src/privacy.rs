//! Personal data on the command line (docs/privacy.md): `tj check --privacy` prints the data
//! map, and `tj privacy export|erase <person>` asks a running server for a person's data or
//! erases it.

use tj_core::vm::program::{Program, SPECIAL_CATEGORIES};

/// The data map: every personal-data field with its category, purposes and readers.
pub fn report(program: &Program) -> String {
    let mut out = String::new();
    for o in program.objects.iter().filter(|o| !o.pii.is_empty()) {
        let read = o.read.as_ref().map_or("server code only".to_string(), |r| r.join(", "));
        out.push_str(&format!("{}  (id: the person's id; read: {read})\n", o.name));
        for p in &o.pii {
            let special = if SPECIAL_CATEGORIES.contains(&&*p.category) { "  special category" } else { "" };
            out.push_str(&format!("  {}  {}{special}  for {}\n", p.field, p.category, p.purposes.join(", ")));
        }
    }
    if out.is_empty() {
        return "no personal data: mark fields with @pii(category, purpose)\n".into();
    }
    out.push_str("The owner sees these fields; other readers and every tool see •••.\n");
    out.push_str("Every compile checks where these values flow (docs/privacy.md); each reveal(value, purpose) lets one path through. Not yet: retention.\n");
    out
}

/// `tj privacy export|erase <person> [--target <url>]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj privacy export|erase <person id> [--target <url>]";
    let (action, subject) = match args {
        [a, s, ..] if (a == "export" || a == "erase") && !s.starts_with("--") => (a.as_str(), s.as_str()),
        _ => return Err(usage.into()),
    };
    let target = match &args[2..] {
        [] => "http://127.0.0.1:4700",
        [flag, url] if flag == "--target" => url.as_str(),
        _ => return Err(usage.into()),
    };
    let url = format!("{}/_tj/privacy", target.trim_end_matches('/'));
    let mut req = ureq::post(&url).set("Content-Type", "application/json");
    if let Some(h) = crate::auth::client_header() {
        req = req.set("Authorization", &h);
    }
    let body = match req.send_string(&serde_json::json!({ "action": action, "subject": subject }).to_string()) {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r.into_string().map_err(|e| e.to_string())?,
        Err(e) => return Err(format!("cannot reach {url}: {e}")),
    };
    let reply: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("bad reply from {url}: {e}"))?;
    if let Some(e) = reply["error"].as_str() {
        return Err(e.to_string());
    }
    println!("{}", serde_json::to_string_pretty(&reply["ok"]).expect("JSON serializes"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_map_lists_every_personal_field() {
        let src = "@server @read(\"owner\", \"support\") object Account {\n  @pii(\"contact\", \"account\", \"billing\") email: string = \"\"\n  @pii(\"health\", \"care\") notes: string = \"\"\n  plan: string = \"free\"\n}\n";
        let r = report(&tj_core::compile(src).unwrap());
        assert!(r.contains("Account  (id: the person's id; read: owner, support)"), "{r}");
        assert!(r.contains("  email  contact  for account, billing\n") && r.contains("  notes  health  special category  for care\n") && !r.contains("plan"), "{r}");
        assert!(report(&tj_core::compile("object A { n: number = 0 }").unwrap()).starts_with("no personal data"));
    }
}
