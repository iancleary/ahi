//! A child owns one Rex session and one durable conversation. Each run has
//! an immutable assignment directory. A single compare-and-publish edge from
//! its predecessor admits either a successor or a close, never both.
use super::{
    broker::{ceiling, denied, restrict, Admission},
    *,
};
use crate::service::{Service, ServiceResult};
use crate::tool_policy::ExecutionMode;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Transition {
    Run(String),
    Close,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Follow {
    pub after: String,
    pub prompt: String,
    pub mode: Option<ExecutionMode>,
}

pub(crate) fn socket(dir: &Path) -> Result<PathBuf> {
    let v: Value =
        read_json(&dir.join("broker.json"))?.ok_or_else(|| Error::input("no owning service"))?;
    v["socket"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| Error::input("invalid service address"))
}
pub(super) fn child_id(dir: &Path, r: &Receipt) -> Result<String> {
    Ok(read_json::<String>(&dir.join("child.json"))?.unwrap_or_else(|| r.id.clone()))
}
pub(super) fn session_matches(r: &Receipt, seen: &Observed) -> bool {
    let target = match (&r.phase, &r.target) {
        (Phase::Launched { terminal, .. }, _) => &terminal.target,
        (_, Target::Rex { .. }) => &r.target,
        _ => return false,
    };
    matches!(target,Target::Rex{session,..} if seen.rex_session.as_ref()==Some(session))
}
pub(super) fn edge(dir: &Path) -> Result<Option<Transition>> {
    read_json(&dir.join("transition.json"))
}
fn link(dir: &Path, transition: Transition) -> Result<()> {
    publish_once(
        dir,
        "transition.json",
        &serde_json::to_vec(&transition).map_err(|_| Error::input("invalid transition"))?,
    )
}
fn latest(dirs: &Directories, id: &str) -> Result<(PathBuf, Receipt)> {
    let mut r = load(dirs, id)?;
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(r.id.clone()) {
            return Err(Error::conflict("cycle in child run records"));
        }
        let dir = assignment_dir(dirs, &r.id)?;
        match edge(&dir)? {
            Some(Transition::Run(next)) => r = load(dirs, &next)?,
            _ => return Ok((dir, r)),
        }
    }
}
pub(super) fn lifecycle(dirs: &Directories, child: &str) -> Result<Value> {
    let root = assignment_dir(dirs, child)?;
    let (dir, r) = latest(dirs, child)?;
    let resident = live(&root);
    let state = if root.join("closed").exists() {
        "closed"
    } else if matches!(edge(&dir)?, Some(Transition::Close)) {
        "close_requested"
    } else if resident && result_exists(&dir) {
        "idle"
    } else if resident {
        "working"
    } else if read_claim(&root)?.is_some() {
        "disconnected"
    } else {
        r.phase.name()
    };
    Ok(
        json!({"state":state,"latest_run":r.id,"transcript":root.join("sessions").join(format!("{child}.jsonl"))}),
    )
}
fn transition_lock(dir: &Path) -> Result<fs::File> {
    let path = dir.join("transition.lock");
    let f = directories::private_file()
        .create(true)
        .open(&path)
        .map_err(|e| Error::io("open", &path, e))?;
    if !ahi_unix::try_lock_exclusive(&f).map_err(|e| Error::io("lock", &path, e))? {
        return Err(Error::conflict("another child transition is in progress"));
    }
    Ok(f)
}
pub(super) fn close(dirs: &Directories, id: String) -> ServiceResult<Value> {
    let first = load(dirs, &id)?;
    let root = assignment_dir(dirs, &child_id(&assignment_dir(dirs, &id)?, &first)?)?;
    let _guard = transition_lock(&root)?;
    let (dir, r) = latest(dirs, &id)?;
    if !matches!(edge(&dir)?, Some(Transition::Close)) {
        link(&dir, Transition::Close)?;
    }
    let child = child_id(&dir, &r)?;
    let root = assignment_dir(dirs, &child)?;
    // Rex may keep a session after its last process exits. Close the exact
    // launch reply only after the active run's terminal result is durable.
    if result_exists(&dir) && !root.join("close.intent").exists() {
        let original = load_dir(&root)?;
        let Phase::Launched { terminal, .. } = &original.phase else {
            return Err(denied("no confirmed dedicated session"));
        };
        let claim = read_claim(&root)?.ok_or_else(|| denied("no verified child claim"))?;
        if !matches!(original.target, Target::RexServer { .. })
            || !session_matches(&original, &claim.observed)
            || claim.observed.rex_block.as_ref() != Some(&terminal.id)
        {
            return Err(denied("refusing to close an unverified session"));
        }
        publish_once(&root, "close.intent", b"")?;
        crate::terminal_host::close_rex_session(terminal)
            .map_err(|e| denied(format!("Rex close not confirmed: {e}")))?;
        publish_once(&root, "closed", b"")?;
    }
    Ok(
        json!({"child":child,"run":r.id,"state":if root.join("closed").exists(){"closed"}else{"close_requested"}}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manage {
    session: String,
    parent_run: String,
    run: String,
    action: super::local::Action,
    mode: Option<ExecutionMode>,
    prompt: Option<String>,
}
pub(super) fn manage(service: &Service, socket: &Path, m: Manage) -> ServiceResult<Value> {
    use super::local::{granted, Action};
    let dirs = service.directories();
    let mut r = load(dirs, &m.run)?;
    let dir = assignment_dir(dirs, &r.id)?;
    let child = child_id(&dir, &r)?;
    let root = assignment_dir(dirs, &child)?;
    let auth: Value =
        read_json(&root.join("authority.json"))?.ok_or_else(|| denied("missing owner"))?;
    if auth["source"]["parent"]["session"].as_str() != Some(&m.session) {
        return Err(denied("not the owning parent session"));
    }
    let p = service.parent_authority(&m.session, &m.parent_run, &r.gateway, &r.model, &r.target)?;
    if !granted(&p.mode, &p.current)
        || !granted(&p.mode, &p.frozen)
        || auth["workspace"].as_str().map(Path::new) != Some(&p.workspace)
    {
        return Err(denied("parent has no grant in this workspace"));
    }
    match m.action {
        Action::Follow => {
            let out = follow(
                service,
                socket,
                Follow {
                    after: m.run,
                    prompt: m.prompt.ok_or_else(|| denied("missing prompt"))?,
                    mode: m.mode,
                },
                Some(p),
            )?;
            Ok(json!({"child":child,"assignment":out["assignment"]["id"],"status":"submitted"}))
        }
        Action::Close => close(dirs, child),
        Action::Cancel => {
            cancel(dirs, &mut r)?;
            Ok(json!({"child":child,"assignment":r.id,"status":"cancel_requested"}))
        }
        Action::Status => {
            let s = status(dirs, &m.run)?;
            Ok(
                json!({"child":child,"assignment":r.id,"state":s["state"],"result":s["result"],"transition":s["transition"]}),
            )
        }
    }
}
pub(super) fn follow(
    service: &Service,
    socket: &Path,
    req: Follow,
    parent: Option<crate::service::ParentAuthority>,
) -> ServiceResult<Value> {
    let dirs = service.directories();
    let prior = load(dirs, &req.after)?;
    let prev = assignment_dir(dirs, &prior.id)?;
    let child = child_id(&prev, &prior)?;
    let root = assignment_dir(dirs, &child)?;
    let _guard = transition_lock(&root)?;
    if !result_exists(&prev) || edge(&prev)?.is_some() {
        return Err(Error::conflict("run is not the idle tip of this child").into());
    }
    if !live(&root) {
        return Err(Error::conflict(
            "child is not attached to a live worker; do not relaunch implicitly",
        )
        .into());
    }
    let auth: Value =
        read_json(&root.join("authority.json"))?.ok_or_else(|| denied("no origin workspace"))?;
    let cwd = Path::new(
        auth["workspace"]
            .as_str()
            .ok_or_else(|| denied("missing workspace"))?,
    );
    let old: Admission =
        read_json(&prev.join("admission.json"))?.ok_or_else(|| denied("missing prior ceiling"))?;
    let (current, policy, frozen) = if let Some(p) = parent {
        (p.mode, p.current, Some(p.frozen))
    } else {
        let (m, p) = crate::chat::operator_authority(cwd, &dirs.state).map_err(denied)?;
        (m, p, None)
    };
    let (mode, new) = restrict(
        current.clone(),
        policy,
        Some(req.mode.unwrap_or_else(|| old.mode.clone())),
    )?;
    let new = if let Some(frozen) = frozen {
        super::broker::workspace_cap(
            cwd,
            dirs,
            &mode,
            ceiling(
                new,
                &restrict(current.clone(), frozen, Some(mode.clone()))?.1,
            )?,
        )?
    } else {
        new
    };
    let (_, cap) = restrict(old.mode.clone(), old.policy, Some(mode.clone()))?;
    let admission = Admission {
        mode: mode.clone(),
        policy: ceiling(new, &cap)?,
        follow_ups: vec![],
    };
    let token: Value =
        read_json(&root.join("broker.json"))?.ok_or_else(|| denied("missing owner"))?;
    if token["socket"].as_str().map(Path::new) != Some(socket) {
        return Err(denied("child belongs to another service"));
    }
    let Phase::Launched { terminal, .. } = prior.phase else {
        return Err(denied("no verified child terminal"));
    };
    let spec = Spec {
        gateway: prior.gateway,
        model: prior.model,
        thinking: prior.thinking,
        prompt: req.prompt,
        timeout_secs: prior.timeout_secs,
        target: prior.target,
    };
    let mut r = reserve_run(dirs, spec, crate::session::uuid(), prior.scratch, false)?;
    let dir = assignment_dir(dirs, &r.id)?;
    let attempt = crate::session::run_id();
    r.expires_at = Some(crate::util::rfc3339::unix_now() + r.timeout_secs as i64);
    r.phase = Phase::Launched { attempt, terminal };
    write_receipt(&dir, &r)?;
    for (name, val) in [
        ("child.json", json!(child)),
        ("admission.json", json!(admission)),
        (
            "authority.json",
            json!({"workspace":cwd,"parent_mode":current,"mode":mode,"tools":admission.policy.advertised_tools(),"turns":1}),
        ),
        ("broker.json", super::broker::credential(socket)?),
    ] {
        write_private_new(&dir.join(name), &serde_json::to_vec(&val).map_err(denied)?)?;
    }
    link(&prev, Transition::Run(r.id.clone()))?;
    Ok(status(dirs, &r.id)?)
}
pub(super) fn lease(dir: &Path) -> Result<fs::File> {
    let path = dir.join("resident.lock");
    let f = directories::private_file()
        .create(true)
        .open(&path)
        .map_err(|e| Error::io("open", &path, e))?;
    if !ahi_unix::try_lock_exclusive(&f).map_err(|e| Error::io("lock", &path, e))? {
        return Err(Error::conflict("child already resident"));
    }
    Ok(f)
}
#[cfg(unix)]
fn live(dir: &Path) -> bool {
    fs::File::open(dir.join("resident.lock"))
        .is_ok_and(|f| matches!(ahi_unix::try_lock_exclusive(&f), Ok(false)))
}
#[cfg(not(unix))]
fn live(_: &Path) -> bool {
    false
}

pub(super) fn recorder(dir: &Path, r: &Receipt) -> Result<crate::session::recorder::Recorder> {
    let child = child_id(dir, r)?;
    let root = assignment_dir(&r.directories.to_directories(), &child)?;
    let store = crate::session::Store::new(&Directories::single_root(root));
    let session = if child == r.id {
        let mut h = crate::session::Header::new(
            &r.scratch.to_string_lossy(),
            &r.gateway,
            &r.model,
            &r.thinking,
        );
        h.id = child;
        store.create(h)
    } else {
        store.hold(&child)
    }
    .map_err(|e| Error::conflict(e.to_string()))?;
    Ok(crate::session::recorder::Recorder::resumed(
        session, "delegate",
    ))
}

// Read human input, not terminal output. EOF detaches input; it is never a close request.
pub(super) fn terminal_input(root: PathBuf, r: Receipt) {
    std::thread::spawn(move || {
        use std::io::{BufRead, Read};
        let dirs = r.directories.to_directories();
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut line = String::new();
            if !matches!((&mut input).take((MAX_PROMPT_BYTES+1) as u64).read_line(&mut line),Ok(n) if n>0 && n<=MAX_PROMPT_BYTES)
            {
                break;
            }
            let msg = line.trim();
            if msg.is_empty() {
                continue;
            }
            let send = || -> ServiceResult<Value> {
                let token: Value = read_json(&root.join("broker.json"))?
                    .ok_or_else(|| denied("missing service endpoint"))?;
                let socket = Path::new(
                    token["socket"]
                        .as_str()
                        .ok_or_else(|| denied("invalid service endpoint"))?,
                );
                let mut c = crate::service::client::Client::listening(&dirs, socket)?;
                if msg == "/close" {
                    return c.call("child.close", json!(r.id));
                }
                let (_, tip) = latest(&dirs, &r.id)?;
                c.call(
                    "child.follow",
                    json!(Follow {
                        after: tip.id,
                        prompt: msg.into(),
                        mode: None
                    }),
                )
            };
            if let Err(e) = send() {
                eprintln!("request refused: {e}");
            }
        }
    });
}
