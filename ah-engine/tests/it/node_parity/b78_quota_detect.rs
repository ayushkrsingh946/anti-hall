//! Corpus for codex-quota-detect (PostToolUse on Agent): >= 30 payload shapes, hand written plus generated.

use super::b78::*;
use super::support::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

include!("b78_quota_detect_tables.rs");

fn agent(resp: Option<Value>, extra: Value, input: Value) -> Value {
    let mut p = json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "session_id": "s1", "cwd": "/tmp", "tool_input": assign(json!({"subagent_type": "codex:codex-rescue", "prompt": "x"}), input)});
    if let Some(r) = resp {
        p["tool_response"] = r;
    }
    assign(p, extra)
}
fn ag(resp: Value) -> Value {
    agent(Some(resp), json!({}), json!({}))
}

fn settings_off(value: Value, payload: Value) -> impl Fn(&super::lab::Lab, &std::path::Path) -> Built + Send + Sync {
    move |lab, root| {
        lab.write(root, "home/.anti-hall/settings.json", json!({"guards": {"codexQuotaDetect": value}}).to_string(), None);
        Built { payload: Some(Payload::Json(payload.clone())), ..Built::default() }
    }
}

pub(crate) fn scenarios() -> Vec<Sc> {
    let mut out: Vec<Sc> = Vec::new();
    for (i, m) in MSGS.iter().enumerate() {
        let m = match i {
            25 => format!("{} out of quota until 2026-10-08T12:00:00Z", "x".repeat(30000)),
            26 => format!("out of quota until 2026-10-08T12:00:00Z {}", "y".repeat(30000)),
            _ => m.to_string(),
        };
        out.push(Sc::json(&format!("str-{i}"), ag(json!(m))));
    }
    // object results (single key objects stringify exactly; multi-key objects defer when they mention quota)
    let until = "out of quota until 2026-10-08T12:00:00Z.";
    out.push(Sc::json("obj-single", ag(json!({"result": until}))));
    out.push(Sc::json("obj-multi-hit", ag(json!({"type": "text", "text": "hit your usage limit, try again at Oct 3rd, 2026 9:11 PM.", "is_error": true}))));
    out.push(Sc::json("obj-multi-clean", ag(json!({"type": "text", "text": "all done, 3 files changed", "is_error": false}))));
    out.push(Sc::json("obj-arr-clean", ag(json!({"content": [{"type": "text", "text": "fine"}, {"type": "text", "text": "also fine"}]}))));
    out.push(Sc::json("obj-arr-hit", ag(json!({"content": [{"type": "text", "text": "out of quota"}]}))));
    out.push(Sc::json("arr-top", ag(json!(["out of quota until 2026-10-08T12:00:00Z"]))));
    out.push(Sc::json("num-resp", ag(json!(5))));
    out.push(Sc::json("bool-resp", ag(json!(true))));
    out.push(Sc::json("null-resp", ag(Value::Null)));
    out.push(Sc::json("empty-resp", ag(json!(""))));
    out.push(Sc::json("tool_output-field", assign(agent(None, json!({}), json!({})), json!({"tool_output": "out of quota until 2026-10-08T12:00:00Z"}))));
    out.push(Sc::json("both-fields", assign(ag(json!("fine")), json!({"tool_output": "out of quota"}))));
    out.push(Sc::json("missing-resp", without(ag(json!("x")), &["tool_response"])));
    // who the agent is
    let types: Vec<Value> = vec![
        json!("codex:codex-rescue"),
        json!("codex-rescue"),
        json!("codex:rescue"),
        json!("codex/rescue"),
        json!("Codex:Codex-Rescue"),
        json!(" codex:codex-rescue "),
        json!("codexrescue"),
        json!("codex-codex-rescue"),
        json!("codex:codex-rescue2"),
        json!("general-purpose"),
        json!(""),
        Value::Null,
        json!(5),
        json!(["codex:codex-rescue"]),
    ];
    for t in types {
        out.push(Sc::json(&format!("type-{t}"), agent(Some(json!("out of quota")), json!({}), json!({"subagent_type": t}))));
    }
    let no_type = |extra: Value| without(agent(Some(json!("out of quota")), json!({}), extra), &[]);
    let mut p = no_type(json!({"agentType": "codex:codex-rescue"}));
    p["tool_input"].as_object_mut().expect("tool_input").remove("subagent_type");
    out.push(Sc::json("agentType", p));
    let mut p = no_type(json!({"agent_type": "codex-rescue"}));
    p["tool_input"].as_object_mut().expect("tool_input").remove("subagent_type");
    out.push(Sc::json("agent_type", p));
    out.push(Sc::json("type-falsy-then-ok", agent(Some(json!("out of quota")), json!({}), json!({"subagent_type": "", "agentType": "codex:codex-rescue"}))));
    out.push(Sc::json("type-num-then-str", agent(Some(json!("out of quota")), json!({}), json!({"subagent_type": 5, "agentType": "codex:codex-rescue"}))));
    // payload shapes
    let q = || ag(json!("out of quota"));
    out.push(Sc::json("not-agent", agent(Some(json!("out of quota")), json!({"tool_name": "Bash"}), json!({}))));
    out.push(Sc::json("no-tool-name", without(q(), &["tool_name"])));
    out.push(Sc::json("no-input", without(q(), &["tool_input"])));
    out.push(Sc::json("input-string", agent(Some(json!("out of quota")), json!({"tool_input": "codex:codex-rescue"}), json!({}))));
    out.push(Sc::json("input-null", agent(Some(json!("out of quota")), json!({"tool_input": null}), json!({}))));
    out.push(Sc::json("input-array", agent(Some(json!("out of quota")), json!({"tool_input": ["codex:codex-rescue"]}), json!({}))));
    out.push(Sc::json("payload-array", json!(["x"])));
    out.push(Sc::raw("payload-null", "null"));
    out.push(Sc::raw("payload-string", "\"out of quota\""));
    out.push(Sc::raw("payload-number", "17"));
    out.push(Sc::raw("malformed-json", "{\"tool_name\":\"Agent\","));
    out.push(Sc::raw("empty-stdin", ""));
    out.push(Sc::raw("whitespace-stdin", "   \n"));
    out.push(Sc::raw("bom", &format!("\u{feff}{}", q())));
    out.push(Sc::json("huge-extra", agent(Some(json!("out of quota")), json!({"junk": "z".repeat(200000)}), json!({}))));
    out.push(Sc::json("unicode-session", agent(Some(json!("out of quota")), json!({"session_id": "\u{e9}\u{1F600}"}), json!({}))));
    // switches
    let until_resp = || ag(json!(until));
    out.push(Sc::json("switch-off-env", until_resp()).env("ANTIHALL_CODEX_QUOTA_DETECT", "0"));
    out.push(Sc::json("switch-on-env", until_resp()).env("ANTIHALL_CODEX_QUOTA_DETECT", "on"));
    out.push(Sc::json("switch-junk-env", until_resp()).env("ANTIHALL_CODEX_QUOTA_DETECT", "maybe"));
    out.push(Sc::setup("switch-off-settings", settings_off(json!(false), q())));
    out.push(Sc::setup("switch-off-settings-str", settings_off(json!("no"), q())));
    out.push(
        Sc::setup("switch-env-beats-settings", settings_off(json!(false), ag(json!("out of quota until 2026-10-08T12:00:00Z"))))
            .env("ANTIHALL_CODEX_QUOTA_DETECT", "1"),
    );
    out.push(Sc::json("switch-plugin-option-off", q()).env("CLAUDE_PLUGIN_OPTION_GUARDS_CODEX_QUOTA_DETECT", "false"));
    out.push(Sc::json("switch-plugin-option-default", q()).env("CLAUDE_PLUGIN_OPTION_GUARDS_CODEX_QUOTA_DETECT", "true"));
    // existing state file merges
    fn put_state(lab: &super::lab::Lab, root: &std::path::Path, txt: &str) -> Built {
        lab.write(root, "home/.anti-hall/codex-availability.json", txt, None);
        Built { payload: Some(Payload::Json(ag(json!("out of quota until 2026-10-08T12:00:00Z.")))), ..Built::default() }
    }
    let state = |txt: &'static str| move |lab: &super::lab::Lab, root: &std::path::Path| put_state(lab, root, txt);
    // the corpus read the clock once, when it built the scenario
    let probe_state = format!("{{\"available\":true,\"checkedAt\":{},\"source\":\"path-probe\"}}", now_ms() - 1000);
    out.push(Sc::setup("state-existing-probe", move |lab, root| put_state(lab, root, &probe_state)));
    out.push(Sc::setup(
        "state-existing-quota",
        state("{\"available\":true,\"checkedAt\":1,\"source\":\"path-probe\",\"quota\":{\"available\":false,\"until\":5,\"reason\":\"old\",\"recordedAt\":1}}"),
    ));
    out.push(Sc::setup("state-corrupt", state("{not json")));
    out.push(Sc::setup("state-array", state("[1,2]")));
    out.push(Sc::setup("state-empty", state("")));
    out.push(Sc::setup("state-extra-keys", state("{\"2\":\"y\",\"7\":\"x\",\"zeta\":1,\"alpha\":[1,2,{\"b\":1,\"a\":2}],\"available\":false}")));
    out.push(Sc::setup("state-proto", state("{\"__proto__\":{\"x\":1},\"a\":1}")));
    out.push(Sc::setup("state-lone-surrogate", state("{\"a\":\"\\ud83d\"}")));
    out.push(Sc::setup("state-big-numbers", state("{\"n\":1e21,\"m\":1.5e-7,\"k\":12345678901234567890,\"z\":-0}")));
    out.push(Sc::setup("state-dir-blocks-file", |lab, root| {
        lab.write(root, "home/.anti-hall/codex-availability.json/x", "y", None);
        Built { payload: Some(Payload::Json(ag(json!("out of quota until 2026-10-08T12:00:00Z.")))), ..Built::default() }
    }));
    // ---- future dates and time zones: the recorded `until` must agree to the millisecond ----
    for tz in ["UTC", "America/New_York", "Asia/Kolkata", "Pacific/Auckland", "Europe/London"] {
        for (i, d) in FUT.iter().enumerate() {
            out.push(Sc::json(&format!("fut-{tz}-{i}"), ag(json!(format!("hit your usage limit, try again at {d}.")))).env("TZ", tz));
        }
    }
    for (i, d) in FUT.iter().enumerate() {
        out.push(Sc::json(&format!("until-{i}"), ag(json!(format!("out of quota until {d}. more text")))).env("TZ", "America/New_York"));
    }
    // ---- generated date strings ----
    let mut r = Rng::new(12345);
    let nfuzz = std::env::var("AH_PARITY_FUZZ").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    for i in 0..nfuzz {
        let d = if r.next() < 0.3 {
            let mut d = format!(
                "{}-{}-{}",
                r.pick(&["2030", "2031", "0001", "9999", "2030", "12030"]),
                r.pick(&["01", "02", "10", "12", "00", "13"]),
                r.pick(&["01", "15", "28", "29", "30", "31", "00", "32"])
            );
            if r.next() < 0.8 {
                let h = *r.pick(&["00", "09", "12", "23", "24", "25"]);
                let mi = *r.pick(&["00", "30", "59", "60"]);
                let se = *r.pick(&["", ":00", ":59", ":61", ":30.250", ":30."]);
                let zo = *r.pick(&["", "Z", "+01:00", "-05:30", "+0100", "z", " UTC", "+25:00"]);
                d.push_str(&format!("T{h}:{mi}{se}{zo}"));
            }
            d
        } else {
            let wd = *r.pick(&WD);
            let mon = *r.pick(&MON);
            let dot = *r.pick(&["", ".", ""]);
            let day = *r.pick(&["1", "3", "15", "28", "29", "30", "31", "0", "32", "3rd", "22nd", "1st", "11th", "3,", "03"]);
            let sep = *r.pick(&[" ", ", ", ","]);
            let yr = *r.pick(&["2030", "2031", "2029", "30", "99", "12345", "1999", "2030"]);
            let time = *r.pick(&[
                "",
                " 9:11 PM",
                " 12:00 AM",
                " 12:30 PM",
                " 0:30 AM",
                " 13:00",
                " 23:59:59",
                " 9:11:30 pm",
                " 9:11 pm utc",
                " 1:00 GMT",
                " 1:00 Z",
                " 25:00",
                " 9:5 PM",
                " 9:11 PM extra",
                " EST",
                " +0200",
                " (UTC)",
            ]);
            format!("{wd}{mon}{dot} {day}{sep}{yr}{time}")
        };
        let prefix =
            *r.pick(&["hit your usage limit, try again at ", "out of quota until ", "exceeded the quota; resets at ", "ran out of quota, try again after "]);
        let suffix = *r.pick(&[".", "", ";", " ok."]);
        let tz = *r.pick(&["UTC", "America/New_York", "Asia/Kolkata", "Pacific/Auckland"]);
        out.push(Sc::json(&format!("gen-{i}"), ag(json!(format!("{prefix}{d}{suffix}")))).env("TZ", tz));
    }
    out
}

/// #243: a retry date the engine cannot read exactly (one only V8 normalises, such as a 30th of February) must still record the
/// outage, with the default cooldown, instead of leaving the shared availability file untouched. Needs no Node.
#[test]
fn an_uncertain_retry_date_still_records_the_outage() {
    for date in ["2030-02-30T12:00:00Z", "2030-10-08T24:00:00Z", "12/25/2026", "Oct 3rd, 2026 9:11 PM"] {
        let s = Scratch::new("q243");
        let root = s.path();
        std::fs::create_dir_all(root.join("home/.anti-hall")).unwrap();
        let (home, tmp) = (root.join("home").to_string_lossy().to_string(), root.join("tmp").to_string_lossy().to_string());
        std::fs::create_dir_all(&tmp).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        let env = env_of(&[("PATH", &path), ("HOME", &home), ("ANTIHALL_TEST_ISOLATION", "1"), ("ANTIHALL_INGEST_DRY_RUN", "1"), ("TMPDIR", &tmp), ("TZ", "UTC")]);
        let payload = ag(json!(format!("hit your usage limit, try again at {date}."))).to_string();
        let before = std::time::SystemTime::now();
        let r = run(ENGINE, &strs(&["check", "codex-quota-detect"]), payload.as_bytes(), &env, &root.to_string_lossy());
        let file = std::fs::read_to_string(root.join("home/.anti-hall/codex-availability.json")).unwrap_or_default();
        let v: Value = serde_json::from_str(&file).unwrap_or_else(|_| panic!("{date}: no availability record written; stdout {:?} stderr {:?}", r.out, r.err));
        assert_eq!(v["quota"]["available"], json!(false), "{date}: {file}");
        let until = v["quota"]["until"].as_u64().unwrap_or(0) as u128;
        let now = before.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        assert!(until > now, "{date}: the recorded cooldown must lie ahead: {file}");
        assert!(r.out.contains("codex-quota"), "{date}: the advisory still goes out: {:?}", r.out);
    }
}

const AVAIL: &str = "home/.anti-hall/codex-availability.json";

/// The lane's one documented difference (#243): when the retry date is one the engine does not read exactly (a shape only V8's
/// parser accepts), the engine records the outage with the default cooldown where Node records the exact date. Everything else
/// (exit code, stderr, every other file, the reason, the advisory apart from the date) must be identical, Node's date must be a real
/// number, and the engine's must be the default cooldown.
fn default_cooldown_for_uncertain_date(n: &Out, e: &Out, ns: &BTreeMap<String, String>, es: &BTreeMap<String, String>) -> bool {
    if n.code != e.code || n.err != e.err {
        return false;
    }
    let (Some(nf), Some(ef)) = (ns.get(AVAIL), es.get(AVAIL)) else { return false };
    let (Ok(mut nv), Ok(ev)) = (serde_json::from_str::<Value>(nf), serde_json::from_str::<Value>(ef)) else { return false };
    // Node's date is a real one still ahead (recorded as a number) or already past (recorded as the default); the engine's is the default
    if ev["quota"]["until"] != json!("<NOW+6H>") {
        return false;
    }
    if nv["quota"]["until"].is_u64() {
        nv["quota"]["until"] = json!("<NOW+6H>");
    }
    if nv != ev {
        return false;
    }
    let others = |m: &BTreeMap<String, String>| m.iter().filter(|(k, _)| k.as_str() != AVAIL).map(|(k, v)| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>();
    if others(ns) != others(es) {
        return false;
    }
    // the advisory names the recorded date; the engine's names the default cooldown
    let lead = "codex-availability.json until ";
    let (Some(from), Some(to)) = (n.out.find(lead).map(|i| i + lead.len()), n.out.find("; Codex is unavailable")) else { return false };
    from <= to && format!("{}(cooldown default){}", &n.out[..from], &n.out[to..]) == e.out
}

pub(crate) const TOLERANCE: Option<Tolerance> = Some(default_cooldown_for_uncertain_date);
